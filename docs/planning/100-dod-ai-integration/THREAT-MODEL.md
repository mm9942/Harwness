# PL-100 — Bedrohungsmodell: DoD × KI

> **Status:** Contract-Draft. Gehört zu [README.md](README.md). Befunde mit Belegen stehen in [INVENTORY.md](INVENTORY.md).

## 1. Schutzgüter

| Gut | Ort | Schutzziel |
|---|---|---|
| Befund-Evidence (`FindingRecord`, volle `SecurityEvidence`) | FindingSpool | Vertraulichkeit (Pfade, IPs, uids); nur der Escalator liest |
| Export `findings.jsonl` v3 | `/var/lib/harw-dod/export` | Integrität; Leser nur `harw-dod-readers` |
| Verdicts | `VerdictStore` (0600, verkettet) | Integrität; sie gelten als Bequemlichkeit, nicht als Audit |
| Labels (Unterdrückung) | signiert über den Approver-Weg | Integrität; ein kompromittierter Job darf nicht unterdrücken |
| TCB-Policy | `/etc/harw-dod/config.toml` (root) | Integrität |
| v2-Schlüssel | `proof_key_dir` | Vertraulichkeit; Escalator und Warden, symmetrisch |
| Escalator- und Warden-Ledger, Nonces | StateDirectories | Integrität, Beständigkeit, S7-Spiegel |
| Freigabe (polkit-Credential) | polkit-Agent | Vertraulichkeit; nie im harw-Prozess |
| Triage-Modellwahl `[dod.triage]` | vertraute Config-Ebenen | Integrität; ein Projekt kann sie nicht ändern |
| Verfügbarkeit der Erkennung | Sentinel, Probes, Dispatch | keine stille Blindheit |

## 2. Angreifer

- **A1 – Prozess auf dem Host** (auch in einem harw-Job): erzeugt Verbindungen, Dateien, Prozessnamen und Pfade und steuert damit Sensortext.
- **A2 – Mitglied von `harw-ipc` oder `harw-warden-clients`:** kann direkt an Sockets senden.
- **A3 – kompromittierter Agent bzw. Prompt-Injection:** Modell- oder Werkzeugausgaben versuchen, Rechte auszuweiten.
- **A4 – kompromittierter harw-Prozess** (harw-uid): kann den harw-Schreibbereich ändern und TTY-Eingaben mitlesen.
- **A5 – Remote-Provider:** sieht die Daten, die an das Triage-Modell gehen.
- **A6 – Lieferkette:** neue Crates (Dispatch, Escalator, Escaper).

## 3. Bedrohungen und Maßnahmen

| # | Bedrohung | Angreifer | Heute | Maßnahme (WP) |
|---|---|---|---|---|
| B1 | Gefälschter Warden-Auftrag (v1, keine Peer-Prüfung) | A2 | **offen** | W01: nur v2, Peer = Escalator-uid, MAC über finding/approver/hold, persistente Nonces (3.1) |
| B2 | Befund-Flut blendet Menschen und KI | A1 | **offen** | Delta, Event-Zeit, Scope, Throttle, Flood-Test als Tor (0.1–0.4) |
| B3 | Prompt-Injection über Sensortext (Prozessnamen, Pfade, SARIF, Peers) | A1 | teilweise (Kappen, uneinheitlich) | ein Escaper inkl. Bidi/Tag-Block (0.8); Templates nur aus IDs/Enums; Begründung nie an Eltern; kein Auto-Turn (I4/I5); Injection-Korpus |
| B4 | Gefälschte Probe-Events über die Sentinel-IPC | A2 | **offen** (SO_PEERCRED nur geloggt) | Peer-Allowlist (0.5) |
| B5 | Modell-Verdict „Confirmed" löst Eingriff aus | A3 | geschützt (es gibt keinen Pfad) | Clamp auf NeedsReview; Operator-Pfad bis 3.2 nur in Tests, danach Feature nur im Escalator, Entscheidung an den Record-Digest gebunden (0.11, 3.2, I1) |
| B6 | Agent bestimmt das Ziel eines Eingriffs | A3 | geschützt (kein Pfad) | Ziel nur aus dem verifizierten Spool-Record, Kernel-ID-Prüfung (I2, 3.2) |
| B7 | Agent gibt sich selbst frei | A3/A4 | — | Entscheidung nur über polkit (vom Escalator angefordert) oder ein unabhängig gestartetes `harw-dod-approve`; Cgroup-Prüfung des Peers; `isatty` kein Beweis (3.3) |
| B8 | Kompromittierter harw-Prozess liest das polkit-Passwort | A4 | — | Weg B: Authentisierung im grafischen polkit-Agenten der Sitzung; Weg A: eigenes Terminal; Same-TTY nur per Einstellung (§3.1) |
| B8a | Kompromittiertes harw schiebt einen Vorschlag unter bzw. fälscht Anzeige oder Bestätigung (Entscheidung nicht an das Angezeigte gebunden) | A4 | — | Anzeige nur durch vertrauenswürdige Oberflächen (Escalator→polkit-Details bzw. unabhängiges `harw-dod-approve`); Autorisierung gebunden an `proposal_digest`; harw startet kein `pkexec`; Kill/Isolate mit eigener polkit-Action und nie im Same-TTY-Modus; Regressionstest „feindlicher Elternprozess/TTY" (3.3) |
| B9 | Unterdrückung von Befunden über Labels oder Verdicts | A3/A4 | — | Verdicts unterdrücken nie (I3); Labels signiert, mit Ablauf, außerhalb des harw-Schreibbereichs (2.5) |
| B10 | Sturm erschöpft das Budget und verdeckt echte Angriffe | A1 | — | partitionierte Budgets, Priorität, reservierte Kapazität, Trip als High-Befund (2.1, I10) |
| B11 | Rückkopplung: ein Eingriff erzeugt neue Anomalien | — | — | Gruppierung je Cgroup, ReTriage nur bei echter Änderung, Grenze für aktive Freezes (2.1, 3.2) |
| B12 | Kill oder Isolation kritischer Prozesse | A3 | geschützt (kein Pfad) | Nur mit Operator-Freigabe über polkit + `irreversible_ack` (O4); Deny-Liste im Warden; nie durch ein Modell oder Playbook (3.1) |
| B13 | Freeze ohne Ende (Verfügbarkeit) | — | — | Vorgabe „bis zur Aufhebung" (O3) ist bewusst; das Menü „Eingriffe" zeigt alle aktiven Freezes dauerhaft an und hebt sie per Freigabe auf; Erinnerungen; Ledger-Abgleich (3.1–3.3) |
| B14 | Sicherheitsdaten gehen an einen Remote-Provider | A5 | — | eigenes Triage-Modell, bewusste Wahl, Doctor zeigt die Lokalität; Evidence-Cards begrenzt und escaped; der Spool verlässt den Host nie (1.9, §3.2) |
| B15 | Projekt-Config ändert Triage-Modell oder Policy | A1 | — | `[dod.triage]` nur aus vertrauten Ebenen; TCB-Policy nur in `/etc` (I6) |
| B16 | Signier-Code in einem modellnahen Prozess | A3/A6 | — | Abhängigkeiten aufgeteilt, Verbots-Zeilen, `cargo tree`-Gate (3.0, I8) |
| B17 | TUI-Freigabe im selben TTY wird abgehört oder gefälscht | A4 | — | **akzeptiertes Restrisiko (O12), begrenzt:** Der Modus ist standardmäßig an, gilt aber nur für umkehrbare Eingriffe (Freeze/Thaw). Kill und Isolation sind dort gesperrt. Der Modus ist per `[dod.approve] tui_tty = false` abschaltbar; Doctor und Mission Control kennzeichnen ihn als schwächer |
| B18 | Sensibles Journal | A1 (Leser) | **offen** | Journal-Hygiene (0.6) |
| B19 | Blinde Sensoren ohne Hinweis | A1 | **offen** | Health mit Fenstern, `sensor_degraded`-Regel, Host-Verifikation (0.13, 1.6, 1.10) |
| B20 | Umgehung durch alphabetische Kürzung oder fehlende pid | A1 | **offen** | Kennzeichen bei Kürzung, Korrelationsfelder (0.15) |
| B21 | Kernel-ID-Wiederverwendung trifft die falsche Cgroup | — | — | Kid + Pfad + `first_seen`, Neuprüfung beim Signieren (0.9, 3.2) |
| B22 | Agent nutzt Host-Werkzeuge nach einem Befund (kill/sudo) ohne Nachfrage | A3 | **offen** (Full ohne Nachfrage, globale `SessionLease`) | Taint-Regel (O1), `SessionLease` mit Ablauf, `job.*` mit ALWAYS_ASK (Phase 4, 3.5) |
| B23 | Manipuliertes BPF-Objekt | A6 | **offen** | SHA-256-Prüfung des Manifests zur Laufzeit (Phase 4) |
| B24 | Ledger-Verlust nach Neustart (Replay) | A2 | — | persistente Nonces, StateDirectory für den Warden (3.1) |
| B25 | Ein gemeinsames `cgroup.freeze`-Bit: `job.resume` hebt eine Warden-Eindämmung desselben Jobs auf, oder die Freigabe des Wardens hebt harws Anhalten auf | A1/A3/A4 | teilweise (harw-Seite umgesetzt; Warden-Marker, Sperre und `/run/harw-warden` fehlen, `job.resume` bleibt auf Hosts mit Warden fail-closed) | Eindämmungsindex des Wardens (Marker je Kernel-ID unter `/run/harw-warden/holds`), geteilte Sperre nach bestem Bemühen, Prüfen, Auftauen und erneutes Prüfen; `job.resume` fail-closed bei unlesbarem Index oder bei Warden-Spur ohne Index; Freezer nur mit beiden Werkzeugen in `ALWAYS_ASK_TOOLS` und delegierter cgroup-v2-Wurzel, mit Warden-Spur bei der Montage Index für die Sitzung auf „Warden erwartet“ festgelegt; dauerhafte Phase `intent`/`applied`: nach einem Neustart taut `job.resume` nur ein belegtes (`applied`) Anhalten auf, ein ungeklärtes bleibt fail-closed bis zur Klärung von Hand (Kosten: Verfügbarkeit, beschreibbares Zustandsverzeichnis nötig); Restrisiken in §4 (3.5) |

## 4. Restrisiken

- Ein Modell kann falsch annotieren. Das wird gemildert, weil Hinweise unabhängig davon immer erscheinen (I3) und Eingriffe einen Menschen brauchen.
- Kleine Modelle sind anfälliger für Injection. Durch I4 und I5 bleibt der Schaden auf eine falsche Annotation begrenzt.
- Die symmetrische v2-MAC: Wer den Schlüssel kompromittiert, kann Freeze/Thaw auslösen. Kill/Isolation verlangen zusätzlich `irreversible_ack`
  im MAC-gebundenen Operator-Entscheid. Das schützt nicht gegen einen kompromittierten Schlüssel, deshalb liegen die Schlüssel root-eigen in `/etc`.
- Same-TTY-polkit (O12, Vorgabe an): Ein kompromittierter harw-Prozess derselben uid könnte die Passworteingabe mitlesen und die Anzeige fälschen.
  Damit kann er eine Freigabe für einen untergeschobenen Freeze/Thaw erschleichen (umkehrbar, sichtbar im Menü „Eingriffe"), aber nie für Kill
  oder Isolation.
- Automatische Playbooks für eigene harw-Jobs (O2, später): ein Fehlalarm kann einen eigenen Job anhalten; begrenzt auf `job.suspend`, umkehrbar.
- `job.suspend`/`job.resume` (Autonomie-Stufe A2, WP 3.5, B25) teilen das eine `cgroup.freeze`-Bit der Job-cgroup mit einer
  Warden-Eindämmung. Abgestimmt wird über Marker im Eindämmungsindex des Wardens (`/run/harw-warden/holds/<Kernel-ID>`; Vertrag bei
  `CONTAINMENT_HOLDS_DIR` in `harw-tool-job/src/freeze.rs`). Es bleiben:
  - **Freeze ohne Marker:** Friert jemand die Job-cgroup ohne Marker ein, *nachdem* harw sie angehalten hat (ein Warden ohne das
    Marker-Protokoll, ein Operator mit `cgroup.freeze` = 1), ist das von harws eigenem Freeze nicht zu unterscheiden; `job.resume` hebt ihn mit
    auf. Ein solcher Freeze *vor* harws Anhalten gilt als fremd: harw übernimmt ihn nicht und hebt ihn nie auf.
  - **Hosts mit Warden heute ohne Fortsetzen:** In diesem Stand legt `harw-dod-warden` noch keine Marker an und nimmt keine Sperre, und
    `deploy/tmpfiles.d/harw.conf` kennt `/run/harw-warden` nicht. Auf einem Host mit Spur eines installierten Wardens
    (`CONTAINMENT_OWNER_EVIDENCE`: `/run/harw/warden.sock`, `/etc/harw-dod/warden.enable`) ist der Index deshalb nicht vertrauenswürdig. Die
    Montage installiert den Freezer trotzdem (Vertrag von `harw-tool-job/src/freeze.rs`, `with_containment_index` in
    `harw-runtime/src/assembly.rs`) und meldet den Zustand einmal auf `info` (`runtime.jobs.resume_refused_until_containment_index`, ohne Pfad).
    `job.suspend` friert ein (die sichere Richtung, Warnung im Log), `job.resume` lehnt jedes Auftauen ab (Fehler `probe`: „the containment index
    cannot be trusted“), bis der Index vertrauenswürdig ist. Bis dahin beenden nur `job.stop` (SIGKILL) oder ein Operator den Freeze; die
    Nachfrage zu `job.suspend` weist darauf nicht hin. Zeigt der Host bei der Montage eine Spur, wird der Index für die Sitzung auf „Warden
    erwartet“ festgelegt (`MarkerDirHolds::owner_expected`); verschwindet die Spur später, bleibt ein fehlendes `/run/harw-warden` trotzdem
    fail-closed. Dasselbe gilt, wenn der Index erst nach der Montage unlesbar oder nicht vertrauenswürdig wird. Damit A2 auf Hosts mit Warden
    auch fortsetzen kann, fehlen ein tmpfiles-Eintrag für `/run/harw-warden` und `/run/harw-warden/holds` (root-eigen, nicht für Gruppe oder
    andere beschreibbar) und das Marker- und Sperrprotokoll in `harw-dod-warden`.
  - **Sperre nur nach bestem Bemühen:** Der Warden versucht `flock(LOCK_EX|LOCK_NB)` auf das Indexverzeichnis genau einmal und dämmt mit oder
    ohne Sperre ein. harw versucht `flock(LOCK_SH)` etwa 100 ms lang (20 Versuche im Abstand von 5 ms) und taut danach ohne Sperre auf, nur durch
    Prüfen, Auftauen und erneutes Prüfen geschützt (Warnung mit `job_id`). Jeder lokale Prozess, auch ein Job unter harws uid, kann die Sperre
    halten. Er verzögert damit jedes `job.resume` um bis zu etwa 100 ms und zwingt es auf den Weg ohne Sperre, blockiert es aber nicht (bewusst,
    sonst könnte jeder Nutzer `job.resume` bzw. die Eindämmung sperren). Fail-closed bleibt `job.resume` bei einem unlesbaren oder nicht
    vertrauenswürdigen Index und bei einem verwaisten Marker, bis der Warden seine Marker beim Start abgleicht oder ein Operator aufräumt.
  - **Restfenster unter Sperrkonkurrenz:** Ohne Sperre kann eine Eindämmung zwischen Prüfung und Auftauen dazukommen. Weil der Warden den Marker
    vor dem Freeze anlegt, findet harws zweite Prüfung ihn, und harw friert sofort wieder ein. Zwischen Auftauen und Wiedereinfrieren läuft der
    Job Mikrosekunden lang weiter. Scheitert das Wiedereinfrieren, meldet harw das auf `error`, und der Vermerk sagt weiter „angehalten".
  - **Umgekehrte Richtung:** Landet harws Freeze zwischen dem Lesen des Bits durch den Warden und dessen eigenem Freeze (harws Prüfung fand noch
    keinen Marker), vermerkt der Warden „war nicht eingefroren". Seine Freigabe hebt dann auch harws Anhalten auf. harws Vermerk sagt weiter
    „angehalten", während der Job läuft. Für die Eindämmung ist das die sichere Seite: `job.stop` beendet einen angehaltenen Job mit SIGKILL.
  - **Angreifer A4:** Ein kompromittierter harw-Prozess schreibt `cgroup.freeze` in seinem delegierten Teilbaum direkt, ohne Nachfrage, ohne
    Eindämmungsindex und ohne Vermerk. Er kann eigene Jobs anhalten und fortsetzen und auch eine Warden-Eindämmung eines Job-Blatts unter seiner
    Wurzel aufheben. Die Nachfrage (`ALWAYS_ASK_TOOLS`, außer unter Full Access) schützt gegen A3, nicht gegen A4. Gegen A4 hält nur ein
    Freeze einer cgroup, deren `cgroup.freeze` die harw-uid nicht schreiben kann (oberhalb ihres delegierten Teilbaums).
  - **Full Access fragt nicht:** Unter `ApprovalMode::FullAccess` (`/permissions set full`) gibt `DefaultApprovalPolicy::review`
    (`harw-registry-defaults/src/lib.rs`) jeden Aufruf frei, auch die aus `ALWAYS_ASK_TOOLS`; Kinder folgen dem Modus der Wurzel ungedeckelt
    (`harw-runtime/src/approval.rs`). `job.suspend` und `job.resume` laufen dann ohne Rückfrage, Stufe A2 „immer mit Nachfrage“ gilt nur in
    den Modi „ask“ und „auto“. Das ist die Nutzerentscheidung vom 2026-09-24 (siehe B22); nur eine ausdrückliche `/permissions`-Deny-Regel
    lehnt dort noch ab. `full_access_lets_suspend_and_resume_through_without_a_question` in `harw-runtime/tests/job_suspend_wiring.rs` hält
    diesen Stand fest, damit eine Änderung sichtbar wird.
  - **Nur auf delegierten cgroup-v2-Hosts:** Die Montage installiert den Freezer nur in der TUI-Sitzung, nur unter Linux, nur über der vom
    Befehls-Port bestätigten delegierten cgroup-v2-Wurzel der Job-cgroups (`[jobs] cgroup_root` → `HARW_CGROUP_ROOT` → eigene delegierte
    cgroup) und nur, wenn beide Werkzeuge in `ALWAYS_ASK_TOOLS` stehen (`install_job_freezer` in `harw-runtime/src/assembly.rs`). Sonst lehnen
    beide Werkzeuge typisiert ab (`SuspendUnavailable`, dessen Text heute pauschal „keine delegierte cgroup-v2-Wurzel“ nennt); Stufe A2
    entfällt dann, es bleibt der Warden-Freeze (Stufe A3).
- Ein globaler Angreifer mit root bleibt außerhalb des Modells.
- Bei einem Remote-Triage-Modell sieht der Provider begrenzte Evidence-Cards. Das ist eine bewusste Entscheidung des Owners.
- Die DoD-Erkennung selbst kann umgangen werden (Kernel-Rootkits, Verkehr ohne TCP-Connect).

## 5. Prüfbare Invarianten

1. N Zyklen über einen konstanten Ring erzeugen genau einen Befund pro Key.
2. Ein Modell-`Confirmed` ergibt nie `Ladder::stage_for = Some`.
3. Der Warden lehnt v1-Belege, fremde Peers, wiederholte Nonces (auch nach einem Neustart), Ziele auf der Deny-Liste sowie Kill/Isolate ohne `irreversible_ack` ab.
4. `harw-dod-approve` lehnt Peers in harw-Session- oder harw-job-Scopes ab, ebenso einen falschen Hash und bei Kill/Isolation eine fehlende Bestätigung „nicht umkehrbar".
4a. Eine Freigabe gilt nur für den `proposal_digest`, den die vertrauenswürdige Oberfläche angezeigt hat. Ein kompromittierter TUI-Prozess kann weder
    den Vorschlag austauschen noch den Bestätigungstext stellen. Kill/Isolate im Same-TTY-Modus werden abgelehnt (Regressionstest).
5. Kein untrusted Byte erscheint in Templates, Hinweisen oder auf dem Approver-Bildschirm; DoD-Daten lösen keinen Auto-Turn aus.
6. Ein Triage-Kind hat genau `DOD_TRIAGE_ROLE_TOOLS` (`DOD_READ_TOOLS` + `DOD_TRIAGE_TOOLS`), Tiefe 0, und steht nicht in `BackgroundChildren`.
7. Ohne `[dod.triage].model` läuft keine automatische Triage, und es gibt keinen Rückfall auf das Session-Modell.
8. Ein Sturm mit 10 000 Befunden hält das Budget ein, ohne High+Observed-Befunde anderer Partitionen zu verdrängen.
9. Unterdrückung gibt es nur durch signierte, ablaufende Labels.
