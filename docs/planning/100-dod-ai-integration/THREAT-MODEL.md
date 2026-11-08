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

## 4. Restrisiken

- Ein Modell kann falsch annotieren. Das wird gemildert, weil Hinweise unabhängig davon immer erscheinen (I3) und Eingriffe einen Menschen brauchen.
- Kleine Modelle sind anfälliger für Injection. Durch I4 und I5 bleibt der Schaden auf eine falsche Annotation begrenzt.
- Die symmetrische v2-MAC: Wer den Schlüssel kompromittiert, kann Freeze/Thaw auslösen. Kill/Isolation verlangen zusätzlich `irreversible_ack`
  im MAC-gebundenen Operator-Entscheid. Das schützt nicht gegen einen kompromittierten Schlüssel, deshalb liegen die Schlüssel root-eigen in `/etc`.
- Same-TTY-polkit (O12, Vorgabe an): Ein kompromittierter harw-Prozess derselben uid könnte die Passworteingabe mitlesen und die Anzeige fälschen.
  Damit kann er eine Freigabe für einen untergeschobenen Freeze/Thaw erschleichen (umkehrbar, sichtbar im Menü „Eingriffe"), aber nie für Kill
  oder Isolation.
- Automatische Playbooks für eigene harw-Jobs (O2, später): ein Fehlalarm kann einen eigenen Job anhalten; begrenzt auf `job.suspend`, umkehrbar.
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
6. Ein Triage-Kind hat genau `DOD_TRIAGE_TOOLS`, Tiefe 0, und steht nicht in `BackgroundChildren`.
7. Ohne `[dod.triage].model` läuft keine automatische Triage, und es gibt keinen Rückfall auf das Session-Modell.
8. Ein Sturm mit 10 000 Befunden hält das Budget ein, ohne High+Observed-Befunde anderer Partitionen zu verdrängen.
9. Unterdrückung gibt es nur durch signierte, ablaufende Labels.
