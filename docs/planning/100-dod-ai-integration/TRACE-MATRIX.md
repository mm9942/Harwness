# PL-100 — Trace-Matrix: offene Punkte → Arbeitspakete

> **Status:** Contract-Draft. Jeder offene Punkt aus den bisherigen Plänen, aus dem Inventar und aus der Vollständigkeits-Kritik wird hier
> einem Arbeitspaket aus [README.md](README.md) zugeordnet oder ausdrücklich als „außerhalb" markiert.

## 1. Inventar-Risiken (INVENTORY.md §H)

| # | Risiko | WP |
|---|---|---|
| H1 | Warden-Eingang fälschbar | 3.1 (W01, erweitert) |
| H2 | Befund-Flut | 0.1–0.4 |
| H3 | kein Dispatch, keine Senke, kein Binden | 1.2, 2.1–2.3, 3.2 |
| H4 | dünne Regeln, keine Baselines, keine Raten | 0.14, 1.6, 1.7, Phase 4 (Tuning) |
| H5 | Sentinel-Landlock wirkungslos | 0.7 |
| H6 | fälschbare Sensor-IPC | 0.5 |
| H7 | Korrelation verloren | 0.1 (Provenienz), 0.15 |
| H8 | blinde Sensoren | 0.13, 1.6, 1.10 |
| H9 | Freigabe-Knöpfe ohne Wirkung | 3.1, 3.2, 3.6 |
| H10 | Host-Werkzeuge missbrauchbar | 3.5, Phase 4 (Taint, Lease, argv) |
| H11 | sensibles Journal, kein manipulationssicheres Audit | 0.6, 3.1/3.2 (Ledger), Phase 4 (S7) |
| H12 | BPF-Manifest ungeprüft | Phase 4 |
| H13 | abweichende Parser | 0.11 |
| H14 | privilegierte Pfade ungetestet | §7 Verifikation (VM-Tests Phase 3), 1.10 |

## 2. PL-97 §9.2 (offen)

| Punkt | WP |
|---|---|
| G1 Tool-Budget | 1.2 (Root behält 3 Werkzeuge), O8 |
| E4 Provenienz bei mehreren Cgroups, `≥`-Badge | 0.1 (Provenienz), 2.4 (Badge) |
| Child-Agenten auf dem Kettenpfad unconfined (E2b) | **außerhalb** (PL-97); Voraussetzung für Phase 3 bei Job-Treffern |
| `harw serve`/`harw web` ignorieren `[jobs] cgroup_root` | 2.2 (betrifft Job-Store-Treffer); Umsetzung in PL-97 |
| `/dod config suggest` statisch | 0.3 (Doctor) und 1.9; die Vorschlagslogik bleibt in PL-97 |
| `--grant-read` fehlt | 3.4 |
| `permission_request!` ohne Konsument | 1.2 (Rechteklasse der `dod.*`), O8 |
| kein SkillInjector für Children | 1.5 |
| `install` aktiviert Dienste automatisch | 3.4 |
| X1b Übersetzung | **außerhalb** |

## 3. PL-96

| Punkt | WP |
|---|---|
| W01: v2 signiert, kein v1, persistenter Nonce-Ledger | 3.1 (Voraussetzung für 3.2–3.4) |
| E12: Warden v1/v2-Mismatch | 3.1 |

## 4. Observability und Sicherheitspläne

| Punkt | WP |
|---|---|
| Gate für den `security.*`-Export nie freigegeben | Phase 4 (Metriken) |
| Runtime emittiert an `NullSink` | Phase 4 |
| Null-Zähler-Registry fehlt (`sensor_parse_errors_total`, `warden_unaudited_actions`) | Phase 4 |
| S7-Audit-Spiegel | Phase 4 |
| Warden linkt kein harw-observe | Phase 4 (unter Beachtung von gate_warden/FORBIDDEN_REACH) |
| Journal-Backend für authlog | 1.8 |

## 5. Vollständigkeits-Kritik (Workflow)

| # | Punkt | WP |
|---|---|---|
| 1 | container und authlog/netlink | 1.8, O11 |
| 2 | Stufe für Zähler → Raten | 0.14 |
| 3 | `BaselineDeviationRule` (Gate, doppelte Metrik, NaN) | 1.7 |
| 4 | Zustand und Semantik für `listener_new` | 1.6 |
| 5 | Voraussetzungen für `persistence_write` | 1.6 |
| 6 | Korrelationsfelder, Kürzung, scanreport | 0.15 |
| 7 | Advisory, intel-scout, context-steward | O11; Advisory → structure-triage (Routing-Tabelle) |
| 8 | Uplink und Cloud Hub, alle Leser | 0.12 (Reader-first), Phase 4 (Vernetzung), PL-99 |
| 9 | Retention-Klassen | 0.12, 2.3 |
| 10 | Observability-Exporter | Phase 4 |
| 11 | Mission Control (Vorschläge, Verdicts, Remote-Kanäle) | 2.4, 3.3 |
| 12 | TraceContext / Zuordnung zu harw-Agenten | Phase 4 (Vernetzung) |
| 13 | LabelStore im modellnahen Vertrauensbereich | 2.5 (signiert, außerhalb des harw-Schreibbereichs) |
| 14 | Warden ohne Persistenz | 3.1 (StateDirectory, Landlock) |
| 15 | Warden-Antworten bei Fehlern | 3.1 |
| 16 | Schlüssel-Bereitstellung und -Rotation | 3.1 |
| 17 | Deployment-Artefakte | 3.4 |
| 18 | Prozesswerkzeuge härten | 3.5, Phase 4 |
| 19 | Annahmen ungeprüft (Landlock, sysfs, authlog, Spool-Gruppe) | 0.7, 1.10, 3.4 (`harw-findings`) |
| 20 | Trace-Matrix für Planpunkte | dieses Dokument |

## 6. Ausdrücklich außerhalb

- Kernel-Rootkit-Erkennung und Integritätsmessung des Kernels.
- Automatische Eingriffe der Stufe A3 im Produktivmodus (O2, nur Shadow).
- A4 (Kill/Isolation) über den Escalator (O4).
- Netzwerkübergreifende Korrelation mehrerer Hosts über den Cloud Hub hinaus (nach PL-99).
