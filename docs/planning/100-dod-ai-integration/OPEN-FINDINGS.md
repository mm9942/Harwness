# PL-100 — Offene Review-Befunde aus PR #168

> **Status:** Arbeitsliste. Stand 2026-11-21, Basis `consolidate/2026-11` (`7037a4f8`) plus Branch `wip/pr168-findings`.
> **Quelle:** die 10 ungelösten Review-Threads von [PR #168](https://github.com/mm9942/Harwness/pull/168)
> (GraphQL `reviewThreads`, `isResolved = false`), am Code dieses Stands nachgeprüft.
> Gehört zu [README.md](README.md). Zeilennummern beziehen sich auf diesen Stand.

## Übersicht

| # | Schwere | Ort | Befund | Stand |
|---|---|---|---|---|
| F1 | High | `harw-dod/crates/harw-probe-fs/src/source.rs:325` | probe-fs bekommt lesbare `O_RDONLY`-Deskriptoren auf root-eigene Persistenzdateien | **offen**, Designentscheidung (unten) |
| F2 | High | `harw-dod/crates/harw-dod-advisory/src/lib.rs:5` | Advisory-Sensor ist in keinem Prozess verdrahtet | **behoben** in `94a3b2da` (Rest: Betrieb der Datenbank) |
| F3 | High | `harw-observe/src/zero_registry.rs:466` | Zero-Registry nicht in der Produktions-Senke | **teilweise behoben** in `94730d30`/`f0fdaace`; Produzenten fehlen (unten) |
| F4 | Medium | `harw-dod/crates/harw-warden/src/warden_factory.rs:112` | O10: Warden nutzt weiter den `nft`-Subprozess, `harw-dod-nft` hat keinen Konsumenten | **offen**, TCB-Entscheidung (unten) |
| F5 | Medium | `harw-tui/src/dod_view.rs:470` | Degradations-Warnungen werden im Hochformat abgeschnitten | **behoben** in `4f08980a` |
| F6 | Medium | `harw-dod/crates/harw-sentinel/src/listener_state.rs:216` | verworfene Listener-Historie wird zur vertrauten Baseline | **offen**, Owner-Entscheidung (unten) |
| F7 | Medium | `harw-dod/crates/harw-sentinel/src/container_plan.rs:321` | Docker-cgroupfs-Layout wird nur beim Start erkannt | **offen** (unten) |
| F8 | Medium | `xtask/src/gate_arch.rs:936` | TCB-Allowlist sieht keine target-spezifischen Kanten | **offen** (Thread „outdated", Befund gilt weiter) |
| F9 | High | `docs/planning/100-dod-ai-integration/README.md` §3.1 | Freigabe nicht im Terminal der kompromittierten TUI | **erledigt im Plan** (Thread kann geschlossen werden) |
| F10 | High | `harw-core-bridge/src/agent_tool.rs` (`budget_exhausted_value`) | Rohtext-Rückweg an der SecurityVerdict-Grenze | **erledigt** (Thread kann geschlossen werden) |

---

## F1 (High) — probe-fs: lesbare Deskriptoren auf überwachte Dateien

**Ort:**
- `harw-dod/crates/harw-probe-fs/src/source.rs:325-327` (`event_f_flags`: `O_RDONLY | O_CLOEXEC | O_NONBLOCK`).
- `RealFanotifySource::read_events` in derselben Datei: der Pfad entsteht nur über `readlink /proc/self/fd/<n>`, der Deskriptor ist aber
  inhaltlich lesbar.
- `deploy/systemd/harw-probe-fs.service:82-83`: `CapabilityBoundingSet`/`AmbientCapabilities=CAP_SYS_ADMIN`.

**Befund (bestätigt):** Der Kernel öffnet den Ereignis-Deskriptor im Namen der Sonde, ohne DAC zu prüfen (`fanotify(7)`). Eine
kompromittierte Sonde liest so `/etc/shadow`, `/root/.ssh/*` oder Cron-Spools, sobald sie geschrieben werden. Landlock lässt diese Wurzeln
absichtlich zu, sonst käme kein Ereignis an.

**Warum hier nicht minimal behoben:**
- `fanotify_init` nimmt kein `O_PATH` an (der Kernel lässt in `event_f_flags` nur `O_ACCMODE`, `O_APPEND`, `O_NONBLOCK`, `O_*SYNC`,
  `O_CLOEXEC`, `O_LARGEFILE` und `O_NOATIME` zu). Jeder Ereignis-Deskriptor ist damit lesbar oder schreibbar.
- Der Weg ohne Deskriptor (`FAN_REPORT_DFID_NAME`, Linux ≥ 5.9, Ereignis mit `FAN_NOFD`) fehlt in `nix` 0.31.3: `InitFlags` kennt kein
  `FAN_REPORT_FID`/`FAN_REPORT_DFID_NAME`, und `Fanotify::read_events` liest nur die Metadaten, keine Info-Records.
- Ein Datei-Handle ließe sich nur über `name_to_handle_at`/`open_by_handle_at` zuordnen. Dafür gibt es in `nix`/`rustix` keine sichere
  Hülle, und der Workspace verbietet `unsafe` (`[workspace.lints.rust] unsafe_code = "forbid"`). `open_by_handle_at` bräuchte zudem
  `CAP_DAC_READ_SEARCH` und lieferte wieder einen Deskriptor.

**Vorgeschlagener Fix (braucht eine Entscheidung zu Abhängigkeit oder `unsafe`-Grenze):**
1. Gruppe mit `FAN_CLASS_NOTIF | FAN_REPORT_DFID_NAME` anlegen. Ereignisse tragen dann Verzeichnis-Handle und Namen, aber keinen Deskriptor.
2. Beim Start für jede Wurzel des `ReadScope` (begrenzte Tiefe und Anzahl) per `name_to_handle_at` eine Tabelle Handle → Pfad bauen.
   Ein Ereignis wird über diese Tabelle zum Pfad. Ein unbekanntes Handle wird als „unaufgelöst" gezählt und als `SensorDegraded` gemeldet.
   `open_by_handle_at` wird nie aufgerufen.
3. Die Info-Records aus dem Rohpuffer von `read(2)` in einem kleinen, geprüften Modul parsen (sicherer Code über Bytes). Den einen
   `name_to_handle_at`-Aufruf entweder upstream in `nix` holen oder in eine winzige, reviewte Crate mit genau dieser `unsafe`-Stelle legen
   (Abhängigkeits-Review wie bei O10).
4. Danach braucht die Landlock-Regel der Sonde kein `ReadFile` mehr auf den überwachten Wurzeln.
5. Regressionstest (root/VM): eine root-eigene `0600`-Datei im überwachten Verzeichnis wird geschrieben. Das Ereignis kommt an, aber die
   Sonde kann den Inhalt nicht lesen (kein Deskriptor, `EACCES` auf `open`).

**Übergangsweise (Owner-Entscheidung):** den Standardbereich ohne `/root/.ssh` und Benutzer-Homes ausliefern, bis 1-5 umgesetzt sind.

## F2 (High) — `harw-dod-advisory` verdrahtet

**Behoben in `94a3b2da` (`wip/pr168-findings`):**
- `harw-sentinel` bekommt `--advisory-db <DIR>` und `--advisory-lockfile <PATH>`. Beide Flags sind absolut, verlangen einander und sind auf
  64 Lockfiles begrenzt (`Cli::validate`, `harw-dod/crates/harw-sentinel/src/cli.rs`).
- Mit beiden Flags registriert `sensors::build_sensors` genau einen unprivilegierten `AdvisorySensor` (`advisory-0`, `ReadWorkspaceGraph`).
- Landlock bekommt je Lockfile eine nur lesbare **Datei**-Regel (nie das Projektverzeichnis) und eine Regel für die Datenbank.
  `sandbox::access_for` gibt Datei-Wurzeln nur Dateirechte.
- Kompositionstest `test_advisory_flags_wire_a_vulnerable_lockfile_into_the_rule_cycle` (`harw-sentinel/src/main.rs`): eine verwundbare
  `tokio@1.8.0` erreicht über die Produktionsbauer die echte Regelauswertung und den Befundexport als ein `structure-drift`-Befund.

**Offen (Betrieb, keine Code-Lücke):**
- Wer hält `rustsec/advisory-db` aktuell? Vorschlag: eigene Unit `harw-advisory-db-update.timer` unter eigenem Konto mit `git pull` nach
  `/var/lib/harw-dod/advisory-db`. Der Sentinel liest nur. Ein veralteter Checkout meldet sich schon heute als `SensorDegraded`
  (`STALE_DATABASE_AGE`).
- Die ausgelieferte `deploy/systemd/harw-sentinel.service` setzt die Flags nicht. Der Sensor ist damit opt-in. Ob er per Vorgabe läuft und
  ob die Werte zusätzlich in `/etc/harw-dod/config.toml` (`[sentinel]`) stehen sollen, entscheidet der Owner.

## F3 (High) — Zero-Registry: Produzenten fehlen noch

**Erledigt (vor diesem Branch):** `94730d30` und `f0fdaace` emittieren die Null-Serien nach dem Anhängen aller Routen.
- Gateway: `DispatchMetrics::install` in `harw-cli/src/observe.rs:474`.
- Sentinel: `install_sensor_metrics` in `harw-dod/crates/harw-sentinel/src/main.rs`, mit dem Produzenten `report_ipc_parse_errors`.
- Kompositionstests in `harw-cli/src/observe.rs`.

**Offen:**
1. **Dispatch-Seite ohne Produzenten.** `TelemetrySinks::dod_metrics` ist `#[allow(dead_code)]` (`harw-cli/src/observe.rs:225-226`).
   `accept_security_verdict` (`harw-core-bridge/src/security_verdict.rs:775`), der Override-Pfad und die DodWatch-Meldungen melden nichts.
   Die sieben Serien bleiben damit dauerhaft null, und „null" ist von „nie verdrahtet" wieder nicht zu unterscheiden.
2. **Warden ohne Handle.** `harw-warden` hängt nicht an `harw-observe`. Er installiert weder `WardenMetrics`, noch registriert er
   `WARDEN_UNAUDITED_VIOLATIONS` in einer `NullCounterRegistry`. Die Serie `warden_unaudited_actions` existiert im Betrieb nie.

**Vorgeschlagener Fix:**
- Ein `Arc<DispatchMetrics>` über die Runtime-Assembly an die `VerdictSink`/Wave-Annahme reichen. Angenommene, abgelehnte und ungebundene
  Verdicts rufen die passenden Meldemethoden über `spawn_blocking` auf. Dazu ein Kompositionstest: ein gebundenes Verdict erhöht
  `dod_triage_verdicts_total` in jedem Standardziel.
- In der Warden-Komposition `WardenMetrics` auf dem eigenen File-Sink installieren, `WARDEN_UNAUDITED_VIOLATIONS` registrieren und
  `warden_unaudited_action` dort aufrufen, wo ein Audit-Append vor einer Aktion scheitert.
- Das berührt die TCB-Allowlist (`xtask/arch-policy.toml`, `[tcb."harw-warden"]`) und braucht deshalb eine Entscheidung, ob
  `harw-observe`/`harw-observe-file` in die Warden-TCB dürfen.

## F4 (Medium) — O10: `nft`-Subprozess statt Rust-Backend

**Ort:**
- `harw-dod/crates/harw-warden/src/warden_factory.rs:107-115` (`network_isolator` → `NftNetworkIsolator::from_cgroup_root`).
- `harw-dod/crates/harw-warden/src/isolation.rs:145` (`DEFAULT_NFT_BINARY = "/usr/sbin/nft"`), `:554` (`NftCommandBackend`),
  `:784` (`impl NetfilterBackend for NftCommandBackend`).
- `harw-dod-nft` wird außerhalb der eigenen Crate nur von `Cargo.toml`, `Cargo.lock` und `xtask/arch-policy.toml` genannt.

**Befund (bestätigt):** Sind Netz-Aktionen erlaubt, startet der Warden `/usr/sbin/nft`. Dafür braucht er `execve` im seccomp-Filter,
`CAP_NET_ADMIN` für den Kindprozess sowie Lese- und Ausführrechte auf Loader, Bibliotheken und `nft`-Konfiguration. Das ist breiter als die in O10
beschlossene In-Process-Grenze.

**Vorgeschlagener Fix:**
- Ein `NetfilterBackend` über `harw_dod_nft` (ein Batch über `NETLINK_NETFILTER`) ersetzt `NftCommandBackend` in `network_isolator`.
- `SandboxNeeds` schrumpft (kein `execve`, keine Pfade unter `/usr/sbin`, `/lib*`, `/etc/nftables*`). Der seccomp-Ratchet streicht `execve`.
- Ein Kompositionstest prüft Backend-Typ und Sandbox-Bedarf der Produktions-Factory.
- Danach der VM-Test aus README §7 „Phase 3" (cgroup v2, Kernel ≥ 5.13) und die noch nicht ausgeführte `strace -f`-Validierung.

**Warum nicht hier:** TCB-Änderung mit Pflicht-VM-Test. Ohne CI und VM ist sie nicht belastbar nachweisbar.

## F5 (Medium) — Hochformat in der DoD-Ansicht

**Behoben in `4f08980a` (`wip/pr168-findings`):** `DodView::render` bricht jede Zeile auf die innere Panelbreite um (`wrap_to_width`). Die Einrückung
bleibt dabei erhalten, und die Warnfarbe übersteht den Umbruch.
Neue Tests in `harw-tui/src/dod_view.rs`:
- `dod_view_wraps_degradation_warnings_in_portrait_width`: 24 Spalten, verifiziert und unverifiziert.
- `dod_view_rewraps_warnings_after_resize`.
- `wrap_to_width_keeps_indent_and_styles_on_continuation_rows`.

Ohne den Fix scheitern beide Ansichtstests.

## F6 (Medium) — Listener-Historie nach Ladefehler

**Ort:** `harw-dod/crates/harw-sentinel/src/listener_state.rs:213-219`. Bei einer kaputten, zu großen, verlinkten oder unlesbaren
Zustandsdatei entsteht `ListenerHistory::new()` (nicht vorbereitet). Der nächste Zyklus lernt alle offenen Listener still. Das
zusätzliche `SensorDegraded` (`record_listener_load_failure`, `main.rs`) nennt diese Listener nicht.

**Entscheidung nötig. Vorschlag A (empfohlen):** ein Wiederanlaufmodus.
- Neuer Konstruktor `ListenerHistory::primed_empty(now)` in `harw-dod/crates/harw-dod-rules/src/listener_history.rs`: leer, aber
  vorbereitet.
- `ListenerState::open` nutzt ihn bei einem Ladefehler. Jeder beim Wiederanlauf offene Listener erscheint dann einmal als `listener-new`
  (fail-closed, einmalig laut) statt still als Baseline.
- Regressionstest: kaputter Zustand plus offener Listener ergibt einen `listener-new`-Befund.

**Vorschlag B:** den Start verweigern bzw. die Datei als `*.corrupt-<zeit>` beiseitelegen, bis ein Operator eingreift.

## F7 (Medium) — Docker-cgroupfs-Layout nur beim Start

**Ort:** `harw-dod/crates/harw-sentinel/src/container_plan.rs:319-321` (`UnsupportedLayout::detected`, einmal in `plan`). Dazu
`main.rs` (`plan_container_sensors` vor Landlock, `record_unsupported_container_layouts` einmal vor der Schleife). Startet der Sentinel vor
dem ersten Docker-Container mit cgroupfs-Treiber, bleibt `<cgroup_root>/docker/<id>` unbeobachtet und wird nie als beeinträchtigt gemeldet.

**Vorgeschlagener Fix:**
- Ein begrenzter Laufzeit-Detektor in der Poll-Schleife, etwa alle N Zyklen. Er prüft `UnsupportedLayout::detected(cgroup_root)` erneut;
  `cgroup_root` ist eine Landlock-Lesewurzel, das geht also auch nach Landlock.
- Je Übergang „nicht erkannt → erkannt" genau ein `SensorDegraded`.
- Regressionstest: `/docker/<id>` erst nach dem Planen anlegen; der laufende Sentinel meldet die Blindheit.

## F8 (Medium) — Architektur-Gate: TCB-Allowlist und Target-Kanten

**Ort:** `xtask/src/gate_arch.rs:936` (Schicht- und TCB-Schleife `for dep in &node.deps`). Sie liest nur den `WorkspaceGraph`.
`ManifestEdges` sieht auch `[target.'…'.dependencies]`, wird aber nur für `[[forbidden_crates]]` benutzt (Moduldoku: „Schichtregel,
TCB-Allowlisten und `*-sys`-Hülle folgen immer dem Graphen"). Ein ausgeliefertes
`[target.'cfg(unix)'.dependencies] harw-dod-rules = { path = "../harw-dod-rules" }` in `harw-dod-warden-types` bliebe grün, obwohl
`[tcb."harw-dod-warden-types"]` nur `harw-macros`/`harw-types` erlaubt.

**Vorgeschlagener Fix:** In der Schleife die Vereinigung aus `node.deps` und `manifests.deps_of(&node.name)` durchlaufen, beschränkt auf
lokale Pakete. Dazu ein roter Test mit genau dieser Target-Kante. Danach einmal das Gate `arch` auf dem echten Baum laufen lassen, weil die
Vereinigung bisher unsichtbare Kanten melden kann.

## F9 (High, im Plan erledigt) — Freigabe nicht im TUI-Terminal

Der Thread (README alt Z. 129) ist „outdated". README §3.1 trennt jetzt Weg A (Operator startet `harw-dod-approve` selbst, außerhalb von
harw), Weg B (TUI sendet nur `RequestApproval{proposal_id}`, der Escalator lässt polkit die eigenen Proposal-Details anzeigen) und den
Same-TTY-Modus (O12: nur umkehrbares Freeze/Thaw, Restrisiko benannt). Der verlangte Regressionstest „feindlicher Elternprozess bzw.
feindliches TTY" steht als Abnahme in README §4 Phase 3.3. Er kann erst mit dem Escalator (WP 3.2) entstehen.
**Empfehlung:** den Thread schließen und den Test als Abnahme von WP 3.3 führen.

## F10 (High, erledigt) — SecurityVerdict-Rohtext bei Budgetende

Der Thread (alt `agent_tool.rs:1843`) ist „outdated" und am Code erledigt:
- `budget_exhausted_value` gibt für `ChildReturnContract::SecurityVerdict` nur
  `{"budget_exhausted": true, "handoff": …, "partial_result_withheld": true}` zurück (`harw-core-bridge/src/agent_tool.rs:3516`).
- Ein validiertes Verdict erreicht das Parent nur als `to_parent_value`-Sicht.
- `agent.result` hält für Triage-Kinder den Rohtext zurück (`verdict_withheld`, `harw-core-bridge/src/agent_result.rs:361`).

**Empfehlung:** den Thread schließen.

---

## Nebenbefund N1 — clippy 1.98 rot in Abhängigkeiten (kein Review-Thread)

Mit der gepinnten Toolchain (`rust-toolchain.toml`: 1.98.1) scheitert
`cargo clippy -p harw-sentinel --all-targets -- -D warnings` bzw. `-p harw-tui` schon in Abhängigkeiten. Dieser Code kam mit PR #168
und ist nie durch CI gelaufen:
- `harw-sandbox/src/process_permit.rs:544` (`clippy::doc_nested_refdefs`);
- `harw-observe/src/zero_registry.rs:356` (`clippy::explicit_auto_deref`);
- `harw-observe/src/routing.rs:1546`, `:1549` (`clippy::type_complexity`, nur Tests).

Die beiden Fehler in `harw-sentinel` selbst sind in `7c69e00a` behoben. `harw-sentinel` und `harw-tui` sind mit `--no-deps` clippy-sauber.
Ein workspaceweiter clippy-Lauf (CI) steht aus.
