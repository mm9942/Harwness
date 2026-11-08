# PL-100 — Harw-DoD × KI: Anomalien → Agenten → Freigabe → Abwehr

> **Status:** Contract-Draft (Konzept, nichts davon umgesetzt). Stand: Code bei `2c131382`.
> **Begleitdokumente:**
> - [INVENTORY.md](INVENTORY.md): Ist-Zustand mit Belegen
> - [THREAT-MODEL.md](THREAT-MODEL.md)
> - [TRACE-MATRIX.md](TRACE-MATRIX.md): offene Planpunkte → Arbeitspakete
>
> **Abhängigkeiten:** PL-96 W01 (v2-Autorisierung, P0), PL-97 (DoD-Suite/Mission Control), PL-99 (Cloud Hub, Uplink).

## 0. Ziel und Entscheidungen

DoD soll durchgehend mit KI genutzt werden:
- Sensordaten und Anomalien gehen gezielt an passende Agenten.
- Agenten werten aus, reichern an und empfehlen.
- Prozesssteuerung und Datenabruf laufen kontrolliert.
- Abwehr-Eingriffe gibt es nur nach Freigabe durch einen Menschen.

**Entscheidungen des Owners (2026-11-08):**

| Thema | Entscheidung |
|---|---|
| Autonomie (Standard) | **Melden + Triage:** Jeder ernste Befund erzeugt sofort einen Hinweis, und eine KI-Triage läuft automatisch. Eingriffe werden nur vorgeschlagen. |
| Freigabe eines Eingriffs | **root/polkit und TUI.** Die TUI *startet* die Freigabe, die Berechtigung prüft polkit (§3.1). |
| Wohin Sicherheitsdaten gehen | Ein **selbst festgelegtes, einzeln einstellbares Triage-Modell** `[dod.triage]` (§3.2). |

## 1. Kurzbefund

Details und Belege stehen in [INVENTORY.md](INVENTORY.md).

**Vorhanden:**
- 10 Sensoren im Sentinel, dazu eBPF für exec und tcp-connect, fanotify (nicht aktiv) und 3 Regeln;
- der Export `findings.jsonl` mit `harw-dod-client`;
- die nur lesenden Werkzeuge `dod.status/findings/sensors`;
- vier Triage-Rollen mit `SecurityVerdict`;
- TUI-Ansichten und Job-Korrelation.

**Lücken, die den KI-Einsatz heute verhindern:**
1. **Befund-Flut:**
   - Der Ring wird nie geleert und alle 5 s komplett neu ausgewertet.
   - Die Regel stempelt `ctx.now`.
   - Der Netzwerkscope ist leer.
   - Folge: Jede ausgehende Verbindung ist alle 5 s ein neuer High-Befund.
2. **Warden fälschbar:** v1-Beleg, keine Peer-Allowlist (PL-96 W01).
3. **„Bruch 2":** kein Dispatch, keine Verdict-Senke, kein Digest zum Binden, kein Escalator.
4. **Dünne Regeln:** keine für exec, Dateischreibzugriffe, Listener, Auth oder Sensorausfall; keine Baselines, keine Raten.
5. **Korrelation fehlt:** keine pid in Befunden oder Flows, keine Provenienz.
6. **Härtung fehlt:**
   - Sensor-IPC wird nicht per Peer geprüft.
   - Das Journal loggt volle Events.
   - Landlock ist vermutlich wirkungslos.
   - Es gibt vier Sanitizer.
   - Das Kontextprogramm der Triage-Rollen ist falsch.

## 2. Leitprinzipien

- **I1 – Modelle beraten, ein Mensch autorisiert außerhalb von harw, der Escalator signiert, der Warden handelt und prüft erneut.**
  - `triage_record` deckelt ein `Confirmed` vom Modell auf `NeedsReview`.
  - `OperatorDecision`/`triage_record_operator` (an den Record-Digest gebunden) gibt es bis WP 3.2 nur in Tests; das Feature
    `operator-decision` kehrt mit `harw-escalator` zurück und darf nur dort aktiviert werden (xtask-Gate `operator-decision`).
- **I2 – Ziele nur aus verifizierten Records.** `SuggestedResponse` hat kein Ziel. Die Cgroup kommt aus dem Spool-Record, den der Escalator selbst prüft:
  Owner, Modus, Digest und beim Signieren erneut die Kernel-ID.
- **I3 – Verdicts annotieren, sie entscheiden nie.** Jeder High-Befund erzeugt immer einen deterministischen Hinweis. Unterdrücken dürfen nur
  menschliche, signierte Labels mit Ablaufdatum.
- **I4 – Drei Kanäle:**
  - Instruktionen: feste ASCII-Templates nur aus UUIDv4-IDs und Enums.
  - Daten: Sensortext nur als `dod.*`-Ausgabe in der Untrusted-Hülle, mit einem gemeinsamen Escaper.
  - Rückkanal: an Eltern nur `{finding_id, classification, severity, suggested_response, verdict_ref}`; die Begründung nur an Menschen.
- **I5 – Kein DoD-Inhalt startet von sich aus einen Modell-Turn**, außer dem dedizierten Triage-Kind. Hinweise sind reine Anzeige.
  Triage-Kinder stehen nicht in `BackgroundChildren`, weil `collect_finished` sonst Auto-Turns erzeugen würde
  (`harw-tui/src/app/background_agents.rs:507-569`).
- **I6 – Die TCB-Policy liegt nur in `/etc/harw-dod/config.toml`** (root-eigen, `load_system_config`). Der harw-Abschnitt `[dod]` ist nur noch beratend
  bzw. für den Doctor.
- **I7 – Der Warden setzt eigene Regeln durch:** Allowlist (Freeze/Thaw), Deny-Liste, eigene `ProofPolicy`. Der MAC bindet `finding_id`,
  `approver` und `hold_secs`.
- **I8 – Prozesse, die mit Modellen sprechen, linken keinen Signier-Code** (harw-runtime/ops/cli/harw; xtask-Gate).
- **I9 – Erst saubere Daten, dann KI.** Der Flood-Regressionstest ist das Tor für jeden Dispatch-Code.
- **I10 – Budgets halten Angriffen stand:** partitioniert nach (uid, Cgroup, Sensor), mit Priorität und reservierter Kapazität
  für High+Observed. Ein Trip ist selbst ein High-Befund.
- **I11 – Reader-first-Rollout** für jedes gemeinsame Schema (Export v3, Config-Schema 2).

**Autonomie-Leiter**

| Stufe | Inhalt | Auslösung |
|---|---|---|
| A0 | Anreichern | automatisch (`mode = "triage"`, Standard nach Entscheidung) |
| A1 | Triage-Verdict + Hinweis | automatisch |
| A2 | eigenen harw-Job anhalten (`job.suspend`) | durch den Menschen, immer mit Nachfrage |
| A3 | Warden-Freeze einer Cgroup | Freigabe durch den Operator (polkit, Kurz-Hash); gefroren bleibt gefroren bis zur Aufhebung (O3) |
| A3-auto | Eigenen harw-Job anhalten durch ein Playbook | zuerst nur Shadow; nach einer Bewährungsphase automatisch, aber nur für eigene harw-Jobs (O2) |
| A4 | Kill oder Netzwerk-Isolation | über denselben Freigabeweg wie ein Freeze (polkit + Kurz-Hash), zusätzlich mit einer ausdrücklichen Bestätigung „nicht umkehrbar" (O4) |

## 3. Zielarchitektur

```
probes ─(SO_PEERCRED-Allowlist)─► harw-sentinel
  Delta-Auswertung · Event-Zeit · Raten-Stufe · NetworkScope aus /etc · FindingThrottle(dedup_key, persistiert)
  ├► FindingSpool (Writer-Thread, ≤64 KiB-Slice, Retention) ── nur der Escalator liest
  └► findings.jsonl v3 {id, instance+seq, record_digest, dedup_key, occurrences, first_seen,
                        kind, hardness, cgroup_kid, scope_display, scope_obfuscated, evidence_card ≤4 KiB}
     health.json (gefensterte Drop-/Overflow-Raten, Spool, Breaker)
         ▼ read-only, Gruppe harw-dod-readers
harw-dod-client v3 + FindingsCursor{instance, seq}
harw-dod-dispatch (pure, Ring A): RouteTable · PartitionedBudget · PriorityQueue · Breaker · IncidentGrouper · ReTriage
         ▼
DodWatch (genau ein Dispatcher je Host; Gateway-Task oder TUI; mode = off | notify | triage)
  ├─ deterministischer Hinweis je High: TUI-Systemzeile, Gateway-Text, Mission Control
  └─ DodTriageSpawner (eigener Spawner, NICHT BackgroundChildren)
       security-*-triage (NoTools + DOD_TRIAGE_ROLE_TOOLS, depth 0, Modell = [dod.triage].model)
       → accept_security_verdict(): parse · cap(2 KiB) · escape · bind(record_digest) → VerdictStore
       → Contain auf zulässiges Ziel → TriageSubmission (IDs, Enums, Verdict-Digest)
                                                         │ Unix-Socket, Peer = harw-uid
harw-escalator (eigene uid; ohne LLM/HTTP/harw-runtime; Policy aus /etc/harw-dod)
  Spool lesen + prüfen → ActionProposal (Prüffrist, Erinnerung, nie Auto-Release)
  ◄─ Freigabe A (unabhängig): Operator startet harw-dod-approve selbst in einer Sitzung, die harw nicht kontrolliert
       (eigenes Terminal/Login/SSH) → Anzeige aus dem Escalator, Kurz-Hash, Hold, polkit auth_admin
  ◄─ Freigabe B (aus der TUI angestoßen): TUI sendet nur „bitte Freigabe für <id> anfordern" an den Escalator
       → der ESCALATOR (nicht harw) ruft polkit CheckAuthorization mit Details aus SEINEM Proposal
         (Aktion, Cgroup, Regel, Kurz-Hash, Digest; Kill/Isolate als eigene Action „nicht umkehrbar")
       → der grafische polkit-Agent der Desktop-Sitzung rendert diese Details → Entscheidung gebunden an den Proposal-Digest
     A und B: Peer-Cgroup nicht unter einer harw-Session oder einem harw-job-Scope; Hold optional (Vorgabe: bis zur Aufhebung)
  → triage_record_operator → Ladder → authorize_freeze(Operator = Peer) → SignedAuthorization v2
  → harw-warden (Peer = Escalator-uid; Allowlist Freeze|Thaw|Kill|Isolate|LiftIsolation je nach Freigabe; Deny-Liste;
                 persistente Nonces; optionaler Hold-Timer taut nur auf; nftables über Rust-Crate; seccomp)
  → Ack → Escalator gleicht den FreezeStore ab
```

### 3.1 Freigabe per polkit und TUI

**Grundsatz (nach Review-Befund vom 2026-11-08):** Was der Operator bestätigt, muss von einer Oberfläche angezeigt werden, die harw nicht
kontrolliert, und die Autorisierung muss an genau diesen Vorschlag gebunden sein (`proposal_digest` über Aktion, Ziel-Cgroup, Kernel-ID,
Befund-ID, Regel, Severity und Hold). Ein kompromittierter harw-Prozess (Angreifer A4) kennt die Vorschlags-IDs und kontrolliert sein eigenes Terminal.
Er darf deshalb weder die Anzeige noch die Bestätigungseingabe stellen.

- **Freigabe A, unabhängig (immer verfügbar, volle Garantie):**
  - Der Operator startet `harw-dod-approve` selbst, in einer Sitzung, die harw nicht kontrolliert: eigenes Terminal-Fenster, anderer Login, SSH.
  - `harw-dod-approve` holt Vorschläge **vom Escalator** (nicht von harw).
  - Es zeigt typisierte, escapte Felder: Aktion, Cgroup-Kid, Pfad, Regel, Severity, Kurz-Hash, Digest.
  - Der Operator tippt den Kurz-Hash ab und wählt optional eine Haltedauer.
  - Danach folgt polkit `auth_admin` ohne keep, mit Action-Details aus dem Escalator.
  - Kill und Isolation (A4) verlangen eine eigene polkit-Action `org.harw.dod.irreversible` und die getippte Bestätigung „nicht umkehrbar".
- **Freigabe B, aus der TUI angestoßen (bequem, gleiche Bindung):**
  - Der Knopf „Freigeben" in Mission Control sendet nur eine Anfrage `RequestApproval{proposal_id}` an den Escalator. harw startet **kein**
    `pkexec` und keinen Freigabedialog in seinem Terminal.
  - Der **Escalator** ruft `CheckAuthorization` (polkit D-Bus) mit `details` aus **seinem** Proposal auf: Aktion, Cgroup, Regel, Kurz-Hash, Digest.
    Der grafische polkit-Agent der Desktop-Sitzung zeigt diese Details an. Der Operator authentisiert also genau das, was der vertrauenswürdige Dialog anzeigt.
  - Wählt ein kompromittiertes harw eine andere `proposal_id`, zeigt der Dialog eben diesen anderen Vorschlag. Der Operator sieht, was er
    freigibt, und harw kann den Inhalt nicht fälschen.
  - Weil der polkit-Dialog keine Freitexteingabe hat, gilt auf diesem Weg die Haltedauer „bis zur Aufhebung" (O3). Eine Haltedauer wird über Weg A gesetzt.
  - Kill und Isolation laufen über die eigene Action `org.harw.dod.irreversible`. Deren Dialogtext nennt die Unumkehrbarkeit.
- **Same-TTY-Modus (O12, `[dod.approve] tui_tty`):**
  - Ohne grafischen polkit-Agenten müsste `pkttyagent` im Terminal der TUI laufen. Dort kann ein kompromittiertes harw die angezeigten Details
    überschreiben, sodass die Bindung an den angezeigten Vorschlag **nicht** gilt.
  - Deshalb erlaubt dieser Modus nur **umkehrbare** Eingriffe: Freeze und Thaw.
  - Kill und Isolation bleiben in diesem Modus gesperrt und laufen nur über Weg A oder Weg B mit grafischem Agenten.
  - Die Vorgabe `true` und das automatische Eintragen bei `install`/`update` bleiben (Owner-Entscheidung O12).
  - Der Doctor und Mission Control kennzeichnen den Modus als „schwächere Bindung".
- **Menü „Eingriffe":**
  - In der TUI (Mission Control) und per `harw dod frozen` werden alle aktiven Freezes und Isolationen gezeigt: Cgroup, Grund, seit wann,
    Freigebender, Hold.
  - Das Aufheben (Thaw bzw. LiftIsolation) läuft über Weg A oder B.
- **Schutz vor Selbstfreigabe:**
  - Der Escalator nimmt `RequestApproval` nur von der harw-uid an. Die Entscheidung kommt immer aus polkit (Weg B) oder aus
    `harw-dod-approve` (Weg A).
  - `harw-dod-approve` lehnt Peers ab, deren Cgroup unter einer harw-Agenten-Session oder einem `harw-job-*`-Scope liegt.
  - `isatty` gilt nicht als Beweis für einen Menschen.
- **harw-cli** bekommt nur ein lesendes `harw dod proposals`.

### 3.2 Triage-Modell

- Einstellungen in `[dod.triage]`: `model = "<provider>/<model>"`, `effort`, `max_tokens`, `max_parallel`.
  - Gelesen wird nur aus vertrauten Ebenen (Home/Profil). Ein Projekt kann das Modell nicht setzen und nicht ändern (Scope-Regel in `harw-config`).
  - Setzen über `/dod model set|show|clear` und die TUI-Settings.
- **Ohne gesetztes Modell** läuft keine automatische Triage. Der Modus fällt auf `notify` zurück, mit einem deutlichen Hinweis.
- Es gibt keinen stillen Rückfall auf das Session-Modell.
- `harw dod doctor` zeigt Provider, Modell und **Lokalität** (loopback/Unix-Socket = lokal, sonst remote). Die Lokalität wird angezeigt,
  aber nicht erzwungen, denn das ist die bewusste Wahl des Owners.

### 3.3 Routing-Tabelle

| Anomalie / Anfrage | Rolle | Ausgabe | Erlaubte Folgeschritte |
|---|---|---|---|
| `egress-flow` | security-egress-triage | Verdict, gebunden an `record_digest` | Hinweis immer. Bei `Contain` auf einen eigenen Job (Treffer im Job-Store): Vorschlag `job.suspend` (Mensch). Bei einer anderen Cgroup im `/etc`-Präfix: Freeze-Vorschlag (Phase 3). Bei FP: Vorschlag für die Allowlist, wirksam nur über ein Label. |
| `listener-new` | security-egress-triage | dito | wie egress |
| `exec-writable-path` | security-endpoint-triage | dito | nur Freeze-Vorschlag |
| `persistence-write` | security-endpoint-triage | dito | Eskalation an einen Menschen; Dateien werden nie angefasst |
| `structure-drift` / scanreport / advisory | security-structure-triage | dito | Hardness Inferred, deshalb wird `Contain` zu `Escalate` herabgestuft |
| `baseline-deviation` / Rate | security-baseline-triage | dito | nur Hinweis bzw. Hinweis auf eigenen Job |
| ≥2 Klassen auf derselben Cgroup in 10 min | gruppiert, Rolle des höchsten Befunds | ein Verdict | wie beim höchsten |
| `scope_obfuscated` | Rolle zur Regel, Severity +1 (höchstens High) | dito | kein Freeze-Vorschlag, nur Mensch |
| SensorDegraded, Spool degraded, Drop-Rate | keine KI | deterministischer Hinweis | Vorschläge tragen die Markierung „Sensoren degradiert" |
| Breaker- oder Budget-Trip | keine KI | High-Befund `dispatch-starved` | High+Observed anderer Partitionen läuft weiter |
| echte Änderung unter bekanntem Key (neuer Port, neue Exe, höhere Hardness, Rate ×4) | ursprüngliche Rolle | erneute Triage | — |
| unbekannte Regel, ID nicht UUIDv4, Digest fehlt, Quelle unverifiziert | kein Dispatch | Zähler | — |
| **Prozesssteuerung** (ein Agent will kill/stop/renice) | — | Vorschlag mit `finding_id` | eigener Job: `job.suspend`/`job.stop` (ALWAYS_ASK); fremde Cgroup: Warden-Vorschlag (A3); Kill/Isolation nur über die CLI eines Menschen |
| **Datenabruf** | Triage-Rollen | `dod.finding`, `dod.related`, `dod.labels` | nur lesend, gebunden, escaped |
| Geplanter Hunt (Phase 4) | security-hunter | `hunt-report/v1` aus Labels und Zählern | Vorschläge zur Prüfung |

## 4. Phasen und Arbeitspakete

### Phase 0 – Saubere Daten (ohne KI)

| WP | Inhalt | Dateien / Ankerpunkte | Test |
|---|---|---|---|
| 0.1 | Delta-Auswertung (Events und Samples) inkl. Attribution; die Regel-API liefert die auslösenden Event-Indizes (Provenienz) | `harw-dod-sentinel/src/buffer.rs` (`events_since`/`samples_since`), `harw-sentinel/src/main.rs:926-933`, `harw-sentinel/src/findings.rs:295-340` | N Zyklen über einen konstanten Ring → genau 1 Befund |
| 0.2 | Event-Zeit statt `ctx.now` | `harw-dod-rules/src/rules/egress_flow.rs:111` | Stempel = `observed_at` |
| 0.3 | `NetworkScope::from_targets(egress_allow_cidrs)`; Loopback wird Low; der Doctor blockiert `triage`, solange die Allowlist leer ist | `harw-sentinel/src/findings.rs:226`, `harw-dod-config` | erlaubte CIDR → 0 Befunde |
| 0.4 | `FindingThrottle`: `dedup_key` = BLAKE3(rule, uid, cgroup_kid, grobes Subjekt: /24 bzw. /64 + Port; bei Drift Datei-Digest + Klasse); persistiert; Ausnahme bei echter Änderung | `harw-sentinel` (neu) | `occurrences`, `first_seen` |
| 0.5 | SO_PEERCRED-Allowlist für Probes | `harw-sentinel/src/ipc.rs:168-176` | Peer mit falscher uid wird abgewiesen |
| 0.6 | Journal-Hygiene | `harw-sentinel/src/main.rs:920-921`, `findings.rs:239-245` | keine Pfade oder IPs im Journal |
| 0.7 | Landlock-Root + `WorkingDirectory` explizit; **Host-Verifikation** (`health.json`, Ruleset-Dump) | `harw-sentinel/src/cli.rs:169`, `sandbox.rs:360`, Unit | Ruleset ≠ „/" |
| 0.8 | Crate `harw-untrusted-text` (Ring A): ein Escaper für Cc/Cf/Zl/Zp/Co/Cn, Tag-Block U+E0000–E007F, Bidi und Variation Selectors; ersetzt vier Sanitizer | `export.rs:149`, `harw-dod-client/src/fsio.rs:84`, `harw-ops/src/dod.rs:388`, `harw-core/src/context_budget.rs:1382` | Injection-Korpus |
| 0.9 | `scope_obfuscated` statt Verwerfen; `cgroup_kid` + Pfad + `first_seen` (Kernel-IDs werden wiederverwendet) | `harw-sentinel/src/export.rs:188-193` | Zero-Width → obfuscated |
| 0.10 | Hardness = `min(rule, event)` | `harw-dod-rules/src/rules/structure_drift.rs:57-63` | — |
| 0.11 | Typestate: Clamp in `triage_record`; `OperatorDecision`/`triage_record_operator` nur in Tests; Feature + Gate kommen mit dem Escalator (WP 3.2), Entscheidung an den Record-Digest gebunden; `triage()` nur für Tests, `test-support` nur aus Dev-Abhängigkeiten; `security_signals` nutzt den Client-Parser | `harw-dod-rules/src/finding.rs:1227-1258` (Clamp), `harw-dod-rules/src/finding.rs:1470-1493` (Operator-Pfad, nur Tests), `harw-runtime/src/security_signals.rs:73` | Modell-`Confirmed` → nie eine Ladder-Stufe; Operator-Entscheidung mit fremdem Record-Digest → abgelehnt; `compile_fail`-Doctests: Operator-Pfad außerhalb von `cfg(test)` nicht linkbar; xtask-Gate `operator-decision` (prüft auch nicht gelistete `path`-Pakete, Gegenprobe gegen `Cargo.lock`): Feature ohne `harw-escalator` deklariert oder von einer anderen Crate angefordert (auch umbenannt, per `workspace = true` geerbt oder über `[features]` weitergeleitet) → rot; Escalator reicht es über eigene `[features]` weiter oder eine andere Crate hängt von ihm ab → rot; `harw-dod-rules` deklariert ein Feature außer `default`/`test-support` → rot; `test-support` außerhalb von `[dev-dependencies]` angefordert oder von einem anderen Feature eingeschaltet → rot |
| 0.12 | Spool + Export v3: Slice ≤64 KiB, Writer-Thread, Retention abgeglichen mit `harw-retention` (`dod_spool`, `sentinel_export`); Reader-first für client, security_signals, security-hub, Uplink | `harw-dod-sentinel/src/spool.rs`, `harw-retention/src/classes.rs:133-147` | Record ≤64 KiB bei vollem Ring |
| 0.13 | Health mit Fenstern (Drop/Overflow je Fenster) | `health.json` | — |
| 0.14 | **Zähler → Raten:** zustandsbehaftete Delta-Stufe vor den Regeln | `harw-sentinel` (neu), `harw-dod-cpu/src/lib.rs:49-56` | Rate korrekt bei einem Zählerüberlauf |
| 0.15 | **Korrelation:** pid/ppid/comm/euid in `Actor`, pid für Flows, Kennzeichen bei Kürzung, ein kaputter Report blendet nicht mehr den ganzen scanreport-Sensor | `harw-probe-bpf/src/collect.rs:325-395`, `harw-dod-flow/src/report.rs:170-176`, `harw-dod-scanreport/src/sensor.rs:211-213` | — |

### Phase 1 – Leseseite und Regeln

- **1.1** `harw-dod-client` v3: `FindingsCursor{instance, seq}`, `EvidenceCard`, strikte UUIDv4-Grammatik, keinen Spool-Leser.
- **1.2** `dod.findings`:
  - neue Filter `id`/`after`;
  - neue Felder `record_digest`, `dedup_key`, `occurrences`, `first_seen`, `kind`, `hardness`, `cgroup_kid`, `scope_obfuscated` sowie die Verdict-Enums;
  - behebt den fehlenden Digest in `harw-ops/src/dod.rs:1263-1275`.

  Neue Liste `DOD_TRIAGE_TOOLS` in `harw-registry-defaults/src/profile.rs`: `dod.finding{id}` (Evidence-Card),
  `dod.related{dedup_key|cgroup_kid, hours}`, `dod.labels{dedup_key}`. Triage-Rollen erhalten über `dod_tools_for_role`
  `DOD_TRIAGE_ROLE_TOOLS` (= `DOD_READ_TOOLS` + `DOD_TRIAGE_TOOLS`, 6 Werkzeuge). **Root behält 3 Werkzeuge** (G1).
- **1.3** Kontextprogramm `agents/context-programs/security-triage.toml`, gilt für die vier Rollen, Golden-IR wird aktualisiert.
- **1.4** Skill `harw-dod`: neuer Abschnitt „Verdict zurückgeben" mit Contract und Beispiel. **1.5** SkillInjector für Child-Sessions (PL-97 offen).
- **1.6** Neue Regeln, `known_rule_id` und Kardinalitätsgrenze werden angepasst (`finding.rs:716-725`):
  - `listener_new`: persistenter Zustand je Cgroup/uid, Regel für den ersten Start, Bind-Adresse und Protokoll werden mit erfasst (`procnet.rs:136-142`);
  - `exec_writable_path`;
  - `sensor_degraded`;
  - `persistence_write`. Voraussetzungen: probe-fs in `harw-dod.target`, echte Scope-Roots, `/proc` lesend im Landlock (auid), Overflow als
    `SensorDegraded`, Zusammenfassen von `FAN_MODIFY`.
- **1.7** `BaselineDeviationRule`:
  - Status-Gate: `Established` eskaliert;
  - doppelte Metrik ergibt einen Fehler statt „last wins";
  - NaN erzeugt keinen Befund (`baseline_deviation.rs:27-46`, `baseline.rs:167`).
- **1.8** Container-Sensor verdrahten. Authlog: Parser für das Kernel-Netlink-Format (`record.rs:326-351`) plus `harw-probe-netlink`, oder
  ausdrücklich zurückgestellt (Entscheidung O11).
- **1.9** `[dod.triage]`-Modelleinstellung: `harw-config` (Scope nur vertraut), `/dod model`, TUI-Settings, Doctor-Anzeige.
- **1.10** Sensor-Wahrhaftigkeit auf einem echten Host prüfen: Alias-Roots für thermal/blockio/gpu, auid, Overflow.

### Phase 2 – Dispatch (nur Empfehlungen)

- **2.1** Crate `harw-dod-dispatch` (pure, Ring A), Verbots-Zeile in `xtask/arch-policy.toml`:
  - `RouteTable` (vollständig);
  - `PartitionedBudget` (uid/cgroup_kid/Sensor + globale Grenze, reservierter Anteil für High+Observed, persistiert über das JobKind-Ledger);
  - `PriorityQueue` (Neuheit, Hardness, bekannter Scope);
  - `BreakerPartition` (Hysterese, Trip → High-Befund);
  - `IncidentGrouper`;
  - `ReTriagePolicy`.
- **2.2** `DodWatch` nach dem `dream_run`-Muster (`harw-runtime/src/dream_run.rs` → neu `harw-runtime/src/dod_watch.rs`, `harw-ops/src/dod_watch.rs`):
  - eigener `DodTriageSpawner`;
  - **ein Dispatcher je Host** (`[dod.dispatch] owner = true` + Host-Lock);
  - der Cursor wird erst nach dem Verdict oder einem terminalen Fehler committet;
  - Wartezeit in der Queue zählt nicht gegen das 120-s-Budget, höchstens 1 Retry;
  - Gateway: überwachter Task mit Backoff (`harw-cli/src/gateway.rs:1007-1013`);
  - TUI: `DodAlertFeed` (erweitert `DodMarkFeed`) und `/dod triage <id>`.
- **2.3** `accept_security_verdict()` gemeinsam für den Wave-Pfad (`harw-core-bridge/src/agent_tool.rs:1804-1818`) und DodWatch:
  - parse → validate → Begründung auf 2 KiB kappen → escapen → `binds()` gegen den exportierten Digest → persistieren → `VerdictRef`.
  - `VerdictStore` (`harw-session-store/src/dod_verdict.rs`): segmentiert, verkettet, 0600, 90 Tage, Retention-Klasse.
- **2.4** Oberflächen:
  - DodView: Fakten zuerst, Begründung eingeklappt und als „vom Modell verfasst" markiert;
  - `harw dod verdicts`;
  - **Mission Control:** Verdict- und Vorschlags-Warteschlange, Badge im Jobs-Fenster, Eintrag im Fenster für fertige Jobs, E5-Kettenmarke;
  - Remote-Kanäle sehen weiterhin nur Status, Befunde und Sensoren (`harw-ops/src/dod.rs:1983-1988`).
- **2.5** Menschliche Labels `/dod label <id> fp|tp`:
  - Unterdrückung nur mit Ablaufdatum.
  - Labels werden **vom Approver-Weg signiert** (polkit wie in §3.1) bzw. außerhalb des harw-Schreibbereichs gespeichert. So kann ein kompromittierter Job
    keine Befunde unterdrücken.
- **2.6** `harw-runtime/tests/rights_matrix.rs`: Ein DodWatch-Kind hat genau `DOD_TRIAGE_ROLE_TOOLS` (`DOD_READ_TOOLS` + `DOD_TRIAGE_TOOLS`), Tiefe 0, und steht nicht in `BackgroundChildren`.

### Phase 3 – Escalator und Freigabe

Voraussetzung ist PL-96 W01. 3.0 und 3.5 können sofort starten.

- **3.0 Abhängigkeiten aufteilen:**
  - `harw-dod-warden-proto` → `-types` (`EscalationStage`, `WardenAction`) + Signierteil;
  - `Ladder` → `harw-dod-ladder`;
  - `TriageSubmission` → `harw-dod-submit-proto`;
  - `harw-plan-bridge` ohne escalate (`harw-plan-bridge/Cargo.toml:47`);
  - Verbots-Zeilen für harw-runtime/ops/cli/harw, Prüfung per `cargo tree`.
- **3.1 W01, erweitert:**
  - nur v2, persistenter Nonce-Ledger, Peer-Allowlist = Escalator-uid;
  - der MAC deckt `finding_id`, `approver` und `hold_secs` ab (`harw-dod-warden-proto/src/signed.rs:10-16`);
  - `ThawCgroup` (ab RuleTriggered) und `LiftIsolation` (nur Escalated) als getrennte Aktionen; `ReleaseCgroup` entfällt (`harw-dod-warden/src/warden.rs:281-285`);
  - eigene `ProofPolicy` und Deny-Liste aus `/etc`. Die Deny-Liste umfasst Warden, Escalator, Sentinel, Probes, `harw-dod.slice`, `system.slice`,
    `init.scope` und User-Session-Scopes;
  - Kill/Isolate nur mit einem Operator-Entscheid, der `irreversible_ack` trägt (O4); der optionale Hold-Timer taut nur auf;
  - nftables über eine Rust-nftables-Crate statt des `nft`-Subprozesses, plus ein seccomp-Filter für den Warden (O10; die Crate braucht ein
    Abhängigkeits-Review und `cargo deny`);
  - **StateDirectory + Landlock-Regel** für Ledger und Timer (heute ist nur `/sys/fs/cgroup` beschreibbar, `harw-warden.service:50`);
  - **immer eine Antwort**, auch bei `ExecutionFailed`/`VerificationFailed` (`warden.rs:117-124`, `ipc.rs:218-224`);
  - **Schlüssel:** Erzeugung, Rotation, Modi, gemeinsame Ablage für Escalator und Warden (`proof_key_dir`, `KeyRing`), mit Bedrohungsangabe zum
    symmetrischen MAC.
- **3.2 `harw-dod/crates/harw-escalator`:**
  - Submission-Socket, Spool-Prüfung (Owner `harw-sentinel`, nicht gruppen- oder weltschreibbar, über Deskriptoren);
  - `ActionProposal` mit Prüffrist und Erinnerung, nie Auto-Release;
  - Grenze für aktive Freezes (Vorgabe 3, in `/etc`);
  - Kernel-ID beim Signieren erneut prüfen;
  - persistierte `OperatorDecision`, eigenes Ledger, neues xtask-Gate.
  - **Operator-Feature kehrt in derselben Änderung zurück:** `harw-dod-rules` deklariert `operator-decision` wieder, und nur `harw-escalator`
    schaltet es ein. `OperatorDecision` ist an den Record-Digest und an den Freigabebeleg (Approver, polkit) gebunden. Das xtask-Gate
    `operator-decision` wertet die Paketidentität aus (Umbenennung, `workspace = true`, `[features]`-Weiterleitung), nicht den Tabellenschlüssel.
    Der Escalator fordert das Feature nur im Abhängigkeitseintrag auf `harw-dod-rules` an, nie über eigene `[features]`, und keine andere Crate
    hängt von `harw-escalator` ab (beides Gate-Regeln).
    Weil Cargo Features je Build vereinheitlicht, verhindert das Gate nur das Anfordern, nicht das Mitbenutzen im gemeinsamen Build;
    die Bestätigung braucht deshalb zusätzlich eine nicht fälschbare, Escalator-eigene Berechtigung.
    Die Gate-Regel für `test-support` (nur aus Dev-Abhängigkeiten, auch für den Escalator) bleibt bestehen. Release-Artefakte entstehen weiterhin je
    Paket mit `-p` und ohne `--all-features` (`.github/workflows/release.yml:101-108`); sonst wären `test-support` bzw. `operator-decision` im Artefakt.
- **3.3 Freigabe gemäß §3.1:**
  - `harw-dod-approve` für Weg A;
  - `RequestApproval` und `CheckAuthorization` mit Proposal-Details im Escalator für Weg B;
  - polkit-Policy mit den Actions `org.harw.dod.freeze` und `org.harw.dod.irreversible`;
  - Same-TTY-Modus nur für Freeze/Thaw;
  - `harw dod proposals` und `harw dod frozen`.
  - **Regressionstest „feindlicher Elternprozess bzw. feindliches TTY":** Ein kompromittierter TUI-Prozess schickt eine fremde `proposal_id`, fälscht
    Terminalausgaben und versucht Kill im Same-TTY-Modus. Erwartet wird: Der polkit-Dialog bzw. der Escalator nennt den echten Vorschlag,
    die Autorisierung gilt nur für dessen Digest, und Kill im Same-TTY-Modus wird abgelehnt.
- **3.4 Deployment:**
  - sysusers für `harw-escalator`, die Approver-Gruppe und `harw-findings` (fehlt heute in `deploy/sysusers.d/harw.conf`);
  - tmpfiles für Spool und Escalator-Socket;
  - Units, polkit-Datei, `gate_privileges`-Budget, `--grant-read`;
  - `install` aktiviert keine Dienste mehr automatisch.
- **3.5 `job.suspend` / `job.resume`:**
  - über `CgroupOps::freeze` (`harw-job-linux/src/cgroup.rs:596`) mit denselben Eigentumsprüfungen wie `job.stop`;
  - in `ALWAYS_ASK_TOOLS` (`harw-registry-defaults/src/lib.rs:489`);
  - `wall_timeout` während eines Suspend ist definiert (`runner.rs:1930`).
- **3.6 `SECURITY.md`:** S1 präzisieren, Invariante „Operator-Ebene außerhalb von harw", Ort der TCB-Policy. `auto_freeze`, `kill_requires_human` und
  `allowed_cgroup_prefixes` in harw-config sind nur noch beratend (der Doctor nutzt sie).

### Phase 4 – Lernen, Härtung, Vernetzung

- **Hunter** (`security-hunter`) arbeitet nur auf Labels und Zählern und gibt Vorschläge zur Prüfung aus.
- **`/dod tune review`:**
  - Baselines liegen in `/etc` (Schema 2, Reader-first);
  - gelernt wird nur aus Fenstern ohne offenen High-Befund, mit Drift-Grenze und Ablaufdatum (≤30 Tage);
  - eine Sensorfamilie muss komplett abgedeckt sein;
  - Dry-Run mit der erwarteten Änderung der Befunde.
- **Shadow-Playbooks** für A2/A3, nur zur Auswertung.
- **Metriken:**
  - `dod_triage_verdicts_total`, `human_overrides_total`, `unbound_verdict_total`, `breaker_trips_total{partition}`, `untriaged_backlog`,
    `warden_unaudited_actions`;
  - Freigabe des `security.*`-Exports, Null-Zähler-Registry;
  - der Warden linkt harw-observe nur, wenn das Gate es erlaubt.
- **Prüfungen:** SHA-256 des BPF-Manifests zur Laufzeit; S7-Spiegel der Escalator- und Warden-Ledger.
- **Prozesswerkzeuge härten:**
  - `process.list` maskiert argv;
  - `SessionLease` mit Ablauf;
  - Cgroup-Filter für `sys.ps`;
  - **Taint-Regel:** Nach `dod.*` im Turn fragen `process.kill`, `host.sudo_exec` und `job.*` nach, außer unter Full Access (Entscheidung O1).
- **Vernetzung (PL-99):**
  - `UplinkEvent::{FindingSummary, HealthBeat}` mit Verdict-Enums;
  - `/v1/posture` über `harw-infra-client`;
  - im Multi-Node-Betrieb: ein Dispatcher je Knoten;
  - `trace_id` im Export und die Zuordnung zu `audit_escalation`/`SudoAuditRecord`, damit Befunde auch harw-eigenen Agenten, Turns und Permits
    zugeordnet werden können.

## 5. Datenfluss für typische Fälle

1. **Anomalie (neue Verbindung eines Agent-Jobs):**
   1. probe-bpf meldet den Flow. Der Sentinel wertet das Delta aus, die Allowlist trifft nicht, der Throttle meldet einen neuen Key.
   2. Es entsteht ein High-Befund (v3), und Mission Control zeigt sofort einen Hinweis.
   3. DodWatch startet egress-triage mit dem Template `{finding_id, rule_id, record_digest}`.
   4. Das Kind ruft `dod.finding` und `dod.related` auf und gibt ein `SecurityVerdict{Contain}` zurück.
   5. Gebunden und gespeichert. Der Job-Store trifft den eigenen Job, deshalb erscheint ein Hinweis „`job.suspend` empfohlen", und der Mensch bestätigt im Dialog.
2. **Prozesssteuerung (fremde Cgroup):**
   1. Bei `Contain` entsteht eine `TriageSubmission`. Der Escalator prüft den Spool und erzeugt einen Vorschlag.
   2. In der TUI: „Freigeben" → der Escalator fordert polkit mit seinen Proposal-Details an → du prüfst den Dialog und authentisierst
      (Haltedauer: bis zur Aufhebung; mit Haltedauer über `harw-dod-approve` in einem eigenen Terminal).
   3. Danach: v2-Signatur → Warden friert ein → Ack → Ledger. Der Timer taut auf, sofern du keine Verlängerung freigibst.
3. **Datenabruf:**
   - Menschen nutzen `harw dod findings|verdicts|proposals`.
   - Agenten nutzen in Root-Sessions `dod.findings` (3 Werkzeuge) und in Triage-Kindern `DOD_TRIAGE_ROLE_TOOLS` (die 3 lesenden plus `dod.finding`, `dod.related`, `dod.labels`).
   - Den Spool liest nur der Escalator.

## 6. Entscheidungen (getroffen am 2026-11-08)

| ID | Frage | Entscheidung | Folge im Design |
|---|---|---|---|
| O1 | Taint-Regel für Prozesswerkzeuge | **nur außerhalb von Full Access** | Nach `dod.*` im Turn fragen `process.kill`, `host.sudo_exec` und `job.*` nach, außer unter Full Access (Phase 4) |
| O2 | automatische Eingriffe | **später für eigene harw-Jobs** | Zuerst Shadow-Playbooks; nach einer Bewährungsphase dürfen sie eigene harw-Jobs automatisch anhalten (`job.suspend`), nie fremde Cgroups |
| O3 | Haltedauer eines Freeze | **bis zur Aufhebung** | Hold ist optional. Neues Menü „Eingriffe" (TUI + `harw dod frozen`) zeigt aktive Freezes/Isolationen und erlaubt die Freigabe |
| O4 | Kill/Isolation | **wie Freeze mit Freigabe** | Gleicher polkit-Weg, zusätzlich die Bestätigung „nicht umkehrbar"; der Warden erlaubt Kill/Isolate nur mit `irreversible_ack` |
| O5 | Ort des Dispatchers | **eigene User-Unit, mit dem Gateway verbunden** | `harw-dod-watch.service` (systemd --user) verbindet sich beim Start automatisch mit dem Gateway; die TUI zeigt „DoD-Watch verbunden/getrennt" |
| O6 | Eigentum am Schema | **nur harw-dod-signals mit DoD-Review** | wie empfohlen |
| O7 | Format der Cgroup-Präfixe | **relativ in /etc, der Doctor normalisiert** | wie empfohlen |
| O8 | Tool-Budget G1 | **3 Werkzeuge in Root, der Rest nur für Triage** | wie empfohlen |
| O9 | Loopback-Egress | **den Nutzer interaktiv im Chat fragen** | Ein Loopback-Befund erzeugt eine Rückfrage in der TUI („erlauben / als Low / normal behandeln"). Die Antwort wird als signiertes Label bzw. Allowlist-Vorschlag gemerkt |
| O10 | nftables und seccomp | **Rust-nftables-Crate + seccomp** | Phase 3.1 ersetzt den `nft`-Subprozess; Abhängigkeits-Review im TCB |
| O11 | zusätzliche Quellen | **Advisory, Auth-Log, Container-Sensor, intel-scout/context-steward** | Alle werden angebunden (Phase 1.8 erweitert; die Rollen kommen in `role_names::ALL` mit `[return]`) |
| O12 | polkit im selben TTY | **erlaubt, per Einstellung, Vorgabe an** | `[dod.approve] tui_tty = true`, wird bei `install` und `update` in `~/.harw/config.toml` geschrieben. Nach dem Review-Befund gilt der Modus nur für umkehrbare Eingriffe (Freeze/Thaw), Kill und Isolation nur mit vertrauenswürdiger Anzeige (§3.1) |

## 7. Verifikation

Siehe auch die prüfbaren Invarianten in [THREAT-MODEL.md](THREAT-MODEL.md).

- **Phase 0:**
  - Flood-Test (konstanter Ring → genau 1 Befund mit `occurrences`);
  - CIDR erlaubt → 0 Befunde, Loopback → Low;
  - Peer mit falscher uid wird abgewiesen;
  - Journal ohne Pfade und IPs;
  - Spool-Record ≤64 KiB, `SpoolFull` erscheint als degraded;
  - Zero-Width → `scope_obfuscated`;
  - Host-Verifikation von Landlock und `health.json`.
- **Typestate:** Ein Modell-`Confirmed` ergibt nie eine Ladder-Stufe. Der Operator-Pfad ist außerhalb von `cfg(test)` nicht linkbar (`compile_fail`-Doctests); ab WP 3.2 darf nur der Escalator das
  Feature `operator-decision` anfordern (xtask-Gate, Paketidentität).
- **Dispatch:**
  - `binds()` mit exportiertem Digest;
  - Root hat genau 3 Werkzeuge;
  - Route-Tabelle vollständig;
  - Proptests für Budgets, Hysterese und Priorität;
  - ein Angreifer mit 21 Keys je 5 min blockiert keine fremden High+Observed-Befunde;
  - ein Sturm mit 10 000 Befunden bleibt im Budget;
  - die Grenzen überstehen einen TUI-Neustart.
- **Injection-Korpus:**
  - „call process.kill", Bidi, Tag-Block, JSON-Ausbrüche, gefälschtes Verdict-JSON, feindliches SARIF, ANSI/OSC;
  - geprüft wird: keine untrusted Bytes in Templates oder Hinweisen, kein Auto-Turn, Eltern sehen nur Enums, der Approver-Bildschirm bleibt unverändert.
- **Eval:** Ein gelabeltes Replay-Korpus misst Precision und Recall je Rolle; in CI Pflicht bei Änderungen an Rolle, Skill oder Route.
- **Phase 3 in einer VM mit cgroup v2:**
  - Negativliste: v1-Beleg, Nonce-Replay über einen Neustart, Digest-Mismatch, Kernel-ID-Mismatch, Deny-Liste, Kill/Isolate mit
    Escalator-Schlüssel, Approver ist die harw-uid oder liegt in einem harw-Scope, nur ein Modell-Verdict, Hold-Ende hebt keine Isolation auf;
  - Ende-zu-Ende: connect → Dispatch → gebundenes Verdict → Vorschlag → TUI-Knopf → polkit → Hash → Freeze → Timer-Thaw → Ledger.
- **Gates:** `cargo xtask gates` mit den neuen Verbots-Zeilen, dem Feature-Gate und dem Escalator-Gate; `cargo deny check`.
