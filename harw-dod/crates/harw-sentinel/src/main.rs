//! `harw-sentinel`: die unprivilegierte Sammelstelle des Ausbauprogramms
//! (Knoten AW2-19).
//!
//! # Das erste vollständig unprivilegierte Binary
//! Dieses Binary hält **keine einzige erhöhte Fähigkeit**. Jede
//! [`harw_dod_cap::Capability`] sitzt in dem Sensor, der sie beim Aufbau
//! seines [`harw_dod_cap::SensorHandle`] gebunden hat
//! ([`harw_dod_sentinel`]-Moduldoku, Abschnitt „Der Sentinel ist
//! unprivilegiert") — dieser Prozess ruft nur `poll()` auf bereits
//! gebundene `dyn harw_dod_signals::Sensor`-Trait-Objekte und liest
//! nirgends selbst aus `/proc`, `/sys`, einem Netlink-Socket oder einer
//! Datei. [`sensors::build_sensors`] registriert ausschließlich Sensoren,
//! deren [`harw_dod_cap::Capability::class`]
//! `harw_dod_cap::CapabilityClass::Unprivileged` ist — die Abnahme dieses
//! Knotens, siehe `sensors`-Moduldoku und
//! [`sensors::tests::test_all_registered_sensors_are_unprivileged`]. Das
//! ist der Grund, warum Bibliothek ([`harw_dod_sentinel`], Knoten AW2-18)
//! und Binary (dieser Knoten) getrennte Knoten sind: die Bibliothek ist
//! ohne Berechtigungen testbar, dieses Binary trägt den Empfangspfad und
//! die Landlock-Bindung.
//!
//! # IPC-Empfangspfad: `SOCK_SEQPACKET`, nicht ein Ringpuffer
//! Die privilegierten Sonden dieses Programms (`harw-probe-fs`,
//! `harw-probe-bpf`, künftig ein `harw-probe-netlink`) senden ihre
//! Ereignisse über einen `SOCK_SEQPACKET`-Unix-Socket an diesen Prozess
//! (Modul [`ipc`]). Drei Gründe, warum kein Ringpuffer im geteilten
//! Speicher: **Nachrichtengrenzen** (ein `recv()` liefert genau ein
//! Ereignis, keinen Byte-Strom, den dieser Prozess neu zerlegen müsste),
//! **`SO_PEERCRED`** (der Kernel verbürgt, welcher Prozess sendet — ein
//! geteiltes Speichersegment hätte keine eingebaute Herkunftsprüfung), und
//! **keine gemeinsam beschriebene Fläche zwischen Berechtigungsklassen**
//! (ein Ringpuffer im `shm`-Segment wäre eine Fläche, die ein
//! privilegierter Sender und dieser unprivilegierte Leser gemeinsam
//! beschreiben — jeder Größen-/Offsetfehler auf einer Seite wird sofort ein
//! Speicherfehler auf der anderen; ein Unix-Socket hält die Adressräume
//! vollständig getrennt). Siehe [`ipc`]-Moduldoku für die vollständige
//! Begründung — und für den Weg, auf dem ein empfangenes Ereignis seit
//! diesem Umbau zusätzlich den Sentinel-Ringpuffer erreicht (Abschnitt
//! „Externe Ereignisse erreichen jetzt den Ringpuffer" unten).
//!
//! **Push-only:** dieser Prozess **empfängt** von den Sonden, **sendet
//! ihnen aber nie etwas** — eine Sonde mit `CAP_SYS_ADMIN`/`CAP_BPF`, die
//! Anweisungen von einem unprivilegierten Prozess entgegennähme, wäre ein
//! Angriffsziel. [`ipc::IpcListener`] und [`ipc::IpcConnection`] haben
//! deshalb keine `send`/`write`-Methode — eine Typ-Eigenschaft, keine
//! Konvention (siehe [`ipc`]-Moduldoku).
//!
//! # Landlock: Degradation statt Startfehler
//! Dieser Prozess beschränkt sich beim Start über
//! [Landlock](https://docs.kernel.org/userspace-api/landlock.html) auf
//! seinen tatsächlichen Lese-/Schreibbereich (Modul [`sandbox`]). Anders
//! als die drei privilegierten Binaries dieses Programms, die ohne ihre
//! jeweilige Kernelfähigkeit hart abbrechen, **degradiert** dieser Prozess
//! bei fehlender Landlock-Unterstützung nur (`SandboxOutcome::NotEnforced`
//! oder `Failed`) — er läuft ohne die zusätzliche Schranke weiter, statt
//! sich selbst abzuschalten. Ein Sicherheitssammler, der auf einem Host
//! ohne Landlock-Unterstützung gar nicht erst startet, verliert auf genau
//! diesem Host jede Beobachtung, die er sonst geliefert hätte — der
//! teurere Fehler. Siehe [`sandbox`]-Moduldoku für die vollständige
//! Begründung. Das resultierende `EventKind::SensorDegraded`-Ereignis war
//! lange nur über `tracing` beobachtbar; seit diesem Umbau erreicht es
//! zusätzlich `Sentinel::buffer()` (Abschnitt „Externe Ereignisse erreichen
//! jetzt den Ringpuffer" unten).
//!
//! # Externe Ereignisse erreichen jetzt den Ringpuffer
//! Zwei Ereignisquellen entstanden bislang außerhalb von
//! [`harw_dod_sentinel::Sentinel::poll_all`] und erreichten
//! `Sentinel::buffer()` deshalb nie: die über [`ipc`] empfangenen
//! `SecurityEvent`s der privilegierten Sonden, und das
//! `EventKind::SensorDegraded`-Ereignis dieses Binaries selbst, falls seine
//! Landlock-Selbstbeschränkung beim Start degradiert (siehe oben). Beide
//! nutzen jetzt [`harw_dod_sentinel::Sentinel::record_external_event`] —
//! den zweiten, dafür vorgesehenen Schreibweg in den Puffer, neben
//! `poll_all` (siehe dortige Crate-Moduldoku, Abschnitt „Zwei Schreibwege
//! in den Puffer").
//!
//! ## Wie die IPC-Inbox `run()` erreicht
//! Die `Arc<Mutex<ipc::IpcInbox>>` wurde früher rein lokal gebaut und beim
//! Verlassen der Startfunktion fallen gelassen — der Empfangsthread füllte
//! eine Inbox, die niemand je wieder las. [`BoundIpc::start_receiving`] gibt
//! das Handle jetzt als [`IpcInboxHandle`] zurück ([`run`] hält ein
//! `Option` davon: `None`, wenn das Binden scheitert oder — auf jeder
//! Nicht-Linux-Plattform — der Empfangspfad gar nicht existiert) und reicht
//! es an jeden [`poll_once`]-Aufruf weiter.
//!
//! ## Binden vor Landlock, Empfangen danach (PL-100 Owner-Review)
//! Landlock beschränkt nur den aufrufenden Thread und Threads, die danach
//! entstehen. [`bind_ipc_if_available`] bindet den Socket deshalb **vor**
//! [`sandbox::restrict_self`] (`/run/harw` ist keine Landlock-Wurzel; ein
//! späteres `bind` dort wäre verboten), startet aber keinen Thread. Die
//! Annahmeschleife und jeder Empfangsthread je Verbindung starten erst in
//! [`BoundIpc::start_receiving`], und das verlangt den
//! [`sandbox::SandboxApplied`]-Beleg, den nur [`sandbox::restrict_self`]
//! ausstellt — ein Empfangsthread außerhalb der Schranke ist damit ein
//! Compile-Fehler. Bis dahin wartende Verbindungen liegen im Backlog des
//! Sockets und werden danach angenommen.
//!
//! ## Reihenfolge im Zyklus (Frage 1)
//! [`poll_once`] ruft zuerst `Sentinel::poll_all` auf, **danach** erst
//! [`drain_external_events`]. Ein in der Inbox wartendes Ereignis landet
//! damit im Puffer **nach** den Sensormessungen desselben Zyklus, nicht
//! davor. Begründung: ein extern gemeldetes Ereignis — eine privilegierte
//! Sonde, die tatsächlich etwas beobachtet hat, oder dieses Binary selbst im
//! Landlock-Fall — ist die seltenere, gezieltere Meldung, während
//! `poll_all` bei jedem Zyklus routinemäßig Samples/Events erzeugt. Bei
//! einem vollen Ereignis-Ring verdrängt jeder weitere Schreibzugriff still
//! den jeweils ältesten Eintrag (siehe Frage 2); würde die Inbox zuerst
//! geleert, wären es exakt diese selteneren, gezielteren externen Ereignisse,
//! die ein nachfolgender `poll_all`-Schwall dieses Zyklus als Erstes
//! verdrängen könnte. Die gewählte Reihenfolge kehrt dieses Risiko um:
//! `poll_all`s routinemäßige Ereignisse stehen näher am Verdrängungsrand.
//!
//! ## Überlauf bei einem vollen Puffer (Frage 2)
//! Die IPC-Inbox fasst [`IPC_INBOX_CAPACITY`] Ereignisse. Übersteigt die
//! Summe aus `poll_all`-Ereignissen und in einem Zyklus entleerten
//! Inbox-Einträgen die konfigurierte Ereignis-Ringkapazität
//! (`SentinelConfig::event_capacity`), verdrängt
//! `Sentinel::record_external_event` still den ältesten Eintrag — **exakt**
//! dasselbe Verhalten wie `poll_all` selbst (siehe dessen Methodendoku in
//! `harw_dod_sentinel`). Das wird hier bewusst hingenommen, statt eine
//! zweite, eigene Kappungsgrenze einzuführen: eine zweite Grenze müsste
//! selbst wieder entscheiden, was sie beim Erreichen verwirft — ein zweites
//! Verdrängungsproblem, kein gelöstes. Ein Schwall aus einer Sonde kann
//! dadurch im Extremfall Sensormessungen desselben Zyklus verdrängen; bei
//! der Standardkapazität (`EvidenceBuffer::DEFAULT_EVENT_CAPACITY` = 4096
//! gegenüber [`IPC_INBOX_CAPACITY`] = 256) ist das praktisch unerreichbar,
//! bleibt aber bei einer bewusst klein konfigurierten Ereignis-Ringkapazität
//! ein Restrisiko — eines, das diese Implementierung nicht stiller macht als
//! jeder andere Schreibzugriff auf denselben Puffer.
//!
//! ## Der Landlock-Fall entsteht vor dem `Sentinel` (Frage 3)
//! [`sandbox::restrict_self`] läuft in [`run`] **vor** `Sentinel::new` — zu
//! diesem Zeitpunkt existiert noch keine Instanz, der ein Ereignis übergeben
//! werden könnte. [`run`] baut das Ereignis trotzdem sofort
//! (`sandbox::landlock_degraded_event`) und hält es in einer lokalen
//! Variable zwischengespeichert; unmittelbar nach `Sentinel::new` — vor dem
//! ersten [`poll_once`]-Aufruf — reicht es es per `record_external_event`
//! nach. Es erscheint dadurch garantiert im allerersten `freeze()`, ohne
//! dass sich die bestehende Startreihenfolge (Socket vor Landlock vor
//! Sensor-Registrierung) ändern musste.
//!
//! ## `tracing::warn!` bleibt zusätzlich stehen (Frage 4)
//! Für beide Ereignisse (IPC wie Landlock) bleibt die vorhandene
//! `tracing`-Meldung unverändert bestehen, **neben** dem neuen Weg in den
//! Puffer. Verschiedene Leser rechtfertigen das: `tracing` bedient den
//! Betreiber, der diesen Prozess in Echtzeit über sein Log beobachtet (etwa
//! über `journalctl -f`, lange bevor der nächste `freeze()` überhaupt
//! zieht); der Ringpuffer-/`freeze()`-Weg bedient das zitierfähige
//! `SecurityEvidence`-Artefakt, das eine spätere, von der Prozesslaufzeit
//! entkoppelte Prüfung liest. Zwei Wege für dieselbe Information sind hier
//! kein Widerspruch, weil kein Leser den anderen ersetzt.
//!
//! ## Warum `Sentinel` nicht von einem `Mutex` umschlossen wird
//! `record_external_event` verlangt `&mut self`, exakt wie `poll_all` — die
//! Bibliothek legt bewusst kein zweites Schutzmodell neben dem von
//! `EvidenceBuffer` an (kein internes Locking, ausschließlich über `&mut
//! self` geschützt). Ein `Mutex<Sentinel>` wäre genau das: ein zweites
//! Schutzmodell, das den Poll-Pfad (diesen Sammelthread) und einen
//! Empfangspfad an einer Stelle serialisierte, an der es niemand erwartet,
//! und zwei Schleifen aneinanderkettete, die heute unabhängig laufen
//! (IPC-Annahme/-Empfang je Verbindung in eigenen Threads, Sammelschleife im
//! Hauptthread). Stattdessen bleibt die Sperre exakt dort, wo sie bereits
//! sitzt: [`ipc::IpcInbox`] hinter ihrem eigenen `Arc<Mutex<_>>`. Der
//! Empfangsthread sperrt nur die Inbox, schreibt sein Ereignis hinein und
//! gibt die Sperre sofort wieder frei; [`drain_external_events`] sperrt
//! dieselbe Inbox erneut, entleert sie **innerhalb dieser einen Sperrspanne**
//! in ein lokales `Vec`, gibt die Sperre frei und übergibt die entleerten
//! Ereignisse danach lock-frei per `&mut self` an den `Sentinel` — aus genau
//! dem Thread, der auch `poll_all` aufruft. `Sentinel` selbst wird damit
//! ausschließlich von diesem einen Thread angefasst; kein zweiter Thread
//! hält je eine Referenz darauf.
//!
//! # Regelauswertung je Zyklus über das Delta (Behebung von Bruch 1, PL-100 WP0.1)
//! [`poll_once`] ruft nach `poll_all` und dem Entleeren der IPC-Inbox
//! [`findings::RuleCycle::evaluate`] auf: das friert über
//! [`harw_dod_sentinel::EvidenceBuffer::freeze_since`] nur die seit dem
//! vorigen Zyklus angekommenen Einträge als
//! [`harw_dod_signals::SecurityEvidence`] ein und übergibt sie an
//! [`findings::report_findings`] — dieselbe injizierte `now`, kein zweiter
//! Systemuhr-Zugriff. Der Ring bleibt Kontextspeicher ([`Sentinel::freeze`]
//! liefert weiterhin seinen vollen Inhalt), aber jedes Ereignis läuft genau
//! einmal durch die Regeln statt in jedem Zyklus erneut (Befund-Flut). Der
//! erlaubte Netzbereich der Regeln stammt aus `egress_allow_cidrs` des
//! aktiven Profils ([`findings::network_scope_from_profile`], WP0.3).
//! Vor den Regeln leitet [`rates::RateStage`] aus den kumulativen Zählern
//! des Deltas (cpu, netcounters, blockio) Raten je Sekunde ab
//! (`"<metric>.rate"`, WP0.14); die Regeln sehen rohe Samples und Raten.
//! Vorher hing hinter `Sentinel`/
//! `EvidenceBuffer` kein Regelwerk: `harw_dod_rules::run_rules` hatte
//! workspace-weit keinen Produktionsaufrufer, jede Fundstelle lag in der
//! Definition selbst, einem Doctest oder einem `#[cfg(test)]`-Modul. Dieser
//! Sentinel bleibt dabei unprivilegiert und setzt nichts durch: er meldet
//! zertifizierte Befunde (`Finding<RuleChecked>`) über denselben
//! `TelemetrySink`, über den er bereits Sensor-Metriken schreibt — siehe
//! [`findings`]-Moduldoku für die vollständige Begründung, das gemessene
//! Abhängigkeitsgewicht dieser Kante, und was zwischen einem Befund und
//! einer durchgesetzten Aktion weiterhin fehlt (Bruch 2, ausdrücklich nicht
//! Teil dieses Knotens).
//!
//! # Entprellung, Journal-Hygiene, gefensterte Gesundheit (PL-100 WP0.4/0.6/0.13)
//! [`run`] baut die Regelauswertung über [`build_rule_cycle`] mit einem
//! [`throttle::FindingThrottle`] (`--finding-cooldown-secs`, sonst
//! `[sentinel] finding_cooldown_secs`, Vorgabe 10 min; Zustand
//! `<home>/finding-throttle.json`): Wiederholungen desselben `dedup_key`
//! werden innerhalb der Abklingzeit nur gezählt, auch über einen Neustart
//! hinweg (der Zustand wird beim Bau geladen). Ereignisinhalte (Pfade,
//! Ziele, Erzeugertext) erscheinen nie im Journal; [`poll_once`] loggt nur
//! die Anzahl je Ereignisart auf `debug`, und ein Befund steht auf `warn`
//! nur mit Regel, Schwere und Zähler (siehe [`findings`]-Moduldoku).
//! `health.json` trägt neben den kumulierten IPC- und Ringzählern deren
//! Deltas des letzten Zyklus und der letzten 60 s (siehe
//! [`health_export`]-Moduldoku).
//!
//! # Listener-Historie und Regelkonfiguration (PL-100 WP1.6)
//! Vor Landlock baut [`rule_config::build`] die
//! [`harw_dod_rules::RuleConfig`] des Hosts: die kanonische
//! `--workspace-root`, jedes verschiedene Home-Verzeichnis aus
//! `/etc/passwd` (abgelehnte wie `/usr/sbin` oder `/` werden nur gezählt)
//! und den flüchtigen Portbereich aus
//! `/proc/sys/net/ipv4/ip_local_port_range` (sonst die Vorgabe).
//! [`build_rule_cycle`] gibt sie allen Regeln und lädt die Listener-Historie
//! `<home>/listener-history.json` ([`listener_state`]; begrenzt gelesen).
//! Die Regeln sehen je Zyklus die Historie vor dem Zyklus, danach lernt sie
//! das Delta mit derselben Konfiguration und wird atomar gespeichert. Eine
//! unbrauchbare gespeicherte Historie bricht den Start nicht ab, wird aber
//! nie still neu gelernt: [`run`] reicht vor dem ersten Zyklus ein
//! `SensorDegraded`-Ereignis ([`listener_state::degraded_event`]) nach, und
//! `health.json` meldet `listener_history` als `degraded`.
//!
//! # Landlock-Ergebnis in `health.json` (PL-100 WP0.7)
//! [`run`] übergibt den Bericht von [`sandbox::SandboxApplied::report`]
//! (Ergebnis, angeforderte und vom Kernel unterstützte ABI-Stufe) an den
//! Gesundheitsexport; jedes Dokument trägt ihn als Objekt `landlock`, und
//! `harw-dod-client` liest es (`HealthReport::landlock`).
//!
//! # `[sentinel]` der DoD-Konfiguration (PL-100)
//! `--probe-peer-user` und `--finding-cooldown-secs` haben keine
//! clap-Vorgabe mehr: ohne Flag gilt der Schlüssel `probe_peer_users` bzw.
//! `finding_cooldown_secs` der optionalen Tabelle `[sentinel]`, ohne ihn die
//! eingebaute Vorgabe ([`effective_probe_peer_users`],
//! [`effective_finding_cooldown_secs`]). Das Flag gewinnt immer; die
//! Herkunft steht auf `info` und im Bericht von `--print-sandbox`.
//!
//! # Probe-Zulassung und Sandbox-Wurzeln (PL-100 WP0.5/WP0.7)
//! [`run`] löst vor dem Binden des IPC-Sockets (und damit vor Landlock, das
//! `/etc` nicht mehr lesen ließe) die Sonden-Konten aus `--probe-peer-user`
//! (sonst `[sentinel] probe_peer_users`) in eine
//! [`peer_allow::PeerAllowlist`] auf (Vorgabe `harw-probe-bpf`,
//! `harw-probe-fs`; uid `0` immer). Die Annahmeschleife schließt jede
//! Verbindung einer anderen uid, bevor sie liest, und zählt sie
//! (`ipc.peers_rejected_total` in `health.json`). `--workspace-root` hat
//! keine Vorgabe mehr; die Landlock-Wurzeln baut [`landlock_roots`], und
//! [`sandbox::prepare_roots`] weist relative Wurzeln und jede Schreibweise
//! von `/` ab — das Arbeitsverzeichnis (`WorkingDirectory=/` in der Unit)
//! geht nie in eine Regel ein. `--print-sandbox` ([`run_print_sandbox`])
//! gibt genau diese wirksamen Wurzeln samt zugelassenen Peer-uids und den
//! Zustandsdateien unter `--home` aus und beendet sich, ohne Socket, Sink
//! oder Landlock anzufassen.
//!
//! # Sensorwahrheit auf dem Host (PL-100 WP1.8/WP1.10)
//! - **sysfs-Aliase.** Thermal-, Blockio- und GPU-Sensor lesen ihre
//!   Klassenverzeichnisse über Alias-Wurzeln
//!   ([`sensors::sysfs_class_scope`]); die Zielpräfixe (`/sys/devices`)
//!   nimmt [`landlock_roots`] als zusätzliche Lesewurzeln auf
//!   ([`sensors::sysfs_alias_prefixes`]), damit die aufgelösten Zugriffe in
//!   der Sandbox bleiben.
//! - **`/proc` lesend.** `--proc-root` ist seit jeher eine Lesewurzel
//!   ([`sandbox::default_roots`]); die loginuid (auid) liest dagegen die
//!   fanotify-Sonde, deren eigene Landlock-Regel `/proc` jetzt umfasst
//!   (`harw-probe-fs`, `landlock.rs`).
//! - **Container.** [`run`] bestimmt vor Landlock die Container-Anordnungen
//!   ([`container_plan::plan`], Schalter [`effective_container_sensor`]:
//!   `--container-sensor` vor `[sentinel] container_sensor` vor `auto`) und
//!   registriert bei `auto` und `on` je bekannter Anordnung einen
//!   `harw_dod_container::ContainerSensor` — auch ohne beim Start erkannte
//!   Laufzeit (daemonloses Podman). Auf einer cgroup-v2-Wurzel ist ein
//!   (noch) fehlendes cgroup-Verzeichnis bei `auto` eine leere Messung,
//!   keine Stilllegung (`on-demand`, systemd legt `machine.slice` erst mit
//!   dem ersten Container an); ohne cgroup-v2-Wurzel meldet sich der Sensor
//!   beim ersten Poll als beeinträchtigt (`required`); eine erkannte, nicht
//!   unterstützte Anordnung (Docker mit `cgroupfs`-Treiber) reicht [`run`]
//!   vor dem ersten Zyklus als `SensorDegraded` nach
//!   ([`record_unsupported_container_layouts`]). `--print-sandbox` nennt
//!   Schalter, Herkunft, Anordnungen samt Verfügbarkeitsregel und nicht
//!   unterstützte Anordnungen.
//!
//! # DoD-Systemkonfiguration und Poll-Taktung
//! [`main`] lädt vor jedem anderen Startschritt (Sink, Socket, Landlock) den
//! gemeinsamen DoD-Vertrag aus [`harw_dod_config`] — denselben, den auch die
//! BPF-Sonde liest ([`load_dod_settings`]):
//!
//! - **Quelle.** Ein expliziter administrativer Pfad (`--config <PATH>`)
//!   geht über [`harw_dod_config::load_config`]; ohne ihn gilt
//!   ausschließlich der feste Pfad [`harw_dod_config::SYSTEM_CONFIG_PATH`]
//!   über [`harw_dod_config::load_system_config`]. Die ausgelieferte Unit
//!   (`deploy/systemd/harw-sentinel.service`) übergibt
//!   `--config /etc/harw-dod/config.toml` ausdrücklich — dort ist eine
//!   fehlende Datei deshalb fail closed, wie bei `harw-probe-bpf`.
//! - **Fehlt die Systemkonfiguration** (`ENOENT` auf irgendeinem Pfadglied),
//!   startet dieser Prozess mit den eingebauten Vorgaben und meldet das per
//!   `tracing::warn!` deutlich — dieses Binary ist unprivilegiert und seine
//!   Sensoren hängen an keinem Profil, ein Host ohne DoD-Installation soll
//!   nicht jede Beobachtung verlieren. Es gibt dann **keinen**
//!   Konfigurations-Digest; eine Bereitschaftsaussage gegenüber der Sonde
//!   darf daraus nicht abgeleitet werden.
//! - **Nicht vertrauenswürdig oder ungültig** (fremder Eigentümer,
//!   gruppen-/weltschreibbar, Symlink, Parse-/Schemafehler, fehlendes oder
//!   unbekanntes `active_profile`, sowie jeder Lesefehler eines explizit
//!   angegebenen Pfads) — **fail closed**: [`main`] beendet sich mit
//!   `ExitCode::FAILURE`, bevor irgendein Socket gebunden wird. Ein
//!   Sicherheitssammler, der still mit einer manipulierten oder halb
//!   gültigen Konfiguration liefe, wäre der teurere Fehler als einer, der
//!   gar nicht startet.
//! - **`SentinelConfig`.** Der DoD-Vertrag (Schema-Version 1) trägt heute
//!   kein Feld für Rückversuchspolitik oder Ringkapazitäten;
//!   [`DodSettings::sentinel_config`] liefert deshalb bewusst
//!   `SentinelConfig::default()`, protokolliert aber Profil und Digest, damit
//!   Sentinel und Sonde dieselbe Auflösung belegen.
//! - **Programmweiter Poll-Abstand.** Auch der DoD-Vertrag legt keinen
//!   Poll-Abstand fest. Maßgeblich ist deshalb das, was die Sammelschleife
//!   tatsächlich taktet: `Sentinel::poll_all` hat keine eigene Uhr, sondern
//!   wird von [`run`] je Runde aufgerufen, getrennt durch
//!   `std::thread::sleep(`[`PollTiming::interval`]`)` — und dieser Wert
//!   stammt aus `--interval-secs` (Vorgabe `cli::DEFAULT_INTERVAL_SECS`).
//!   [`PollTiming`] ist die eine Stelle, aus der jedes zeitgebundene
//!   Sensorfenster abgeleitet wird: ein Rückschaufenster ist
//!   `max(Vorgabe, 2 × Poll-Abstand)` ([`PollTiming::lookback_for`]), damit
//!   aufeinanderfolgende Polls sich stets überlappen statt eine Lücke zu
//!   lassen — selbst wenn eine Runde sich um bis zu einen vollen Abstand
//!   verspätet.
//!
//! # Exports for harw (PL-97 D1)
//! Two optional, unprivileged outputs leave this process: the append-only
//! findings export (`--findings-export`, module [`export`], schema version
//! 3) and the health document (`--health-export`, module
//! [`health_export`]), which defaults to the sibling `health.json` of the
//! findings export and is replaced atomically at the end of every
//! [`poll_once`] cycle. Both directories are added as read-write Landlock
//! roots before [`sandbox::restrict_self`]; neither needs a capability.
//! Before any writer starts (right after the telemetry sink has created
//! `--home`), [`check_export_destinations`] refuses startup (fail-closed,
//! [`error::SentinelBinError::Destinations`]) when two of the sentinel's
//! files alias each other by resolved identity: the findings export and its
//! rotated generations, the health document and its temporary sibling, and
//! the throttle state `<home>/finding-throttle.json` and its temporary
//! sibling, and the listener history `<home>/listener-history.json` and its
//! temporary sibling (PL-100 owner review: an alias would let one atomic
//! rename replace the other writer's file every cycle). When two of them name
//! different parent directories, both directories must already exist at
//! startup: an unresolvable parent cannot be proven distinct, so the
//! sentinel refuses to start instead of retrying later.
//! Without both flags nothing is written and the cycle is unchanged (the
//! health snapshots around `poll_all` are only taken when the health export
//! is enabled).
//!
//! # Meta-Metrik `sensor_parse_errors_total` (PL-100)
//! [`run`] legt über den Telemetrie-Sink einen [`harw_observe::RoutingSink`]
//! und installiert darauf das eine [`harw_observe::SensorMetrics`]-Handle
//! dieses Prozesses ([`install_sensor_metrics`]; die Nullserie geht sofort an
//! den File-Sink). Jede IPC-Nachricht einer zugelassenen Sonde, die nicht
//! dekodierbar oder zu groß ist, zählt der Empfangsthread in der Inbox;
//! [`report_ipc_parse_errors`] meldet den Block einmal je Zyklus als
//! `sensor_parse_errors_total` und mit einer `warn`-Zeile, die nur die
//! Anzahl trägt — nie Nutzlast, Peer oder Fehlertext.
//!
//! # Nicht-Linux-Builds
//! Der IPC-Empfangspfad ([`ipc`]) und Landlock ([`sandbox`]) gibt es nur auf
//! Linux; `landlock` und `rustix` sind deshalb Linux-Abhängigkeiten, und auf
//! anderen Plattformen gelten die `cfg(not(target_os = "linux"))`-Fassungen
//! (kein Empfang, `SandboxOutcome::NotEnforced`). Die Zustandsdateien
//! öffnet nur die Linux-Fassung mit `O_NOFOLLOW` am Deskriptor
//! (`health_export::read_regular_bounded`); anderswo prüft
//! `symlink_metadata` unmittelbar vor dem Öffnen. Die Unix-Rechtepfade
//! (Exportmodi, Spool-Verzeichnis) sind `cfg(unix)` mit einem No-Op
//! außerhalb von Unix. Prüfziel für den Nicht-Linux-Build:
//! `cargo check -p harw-sentinel --target x86_64-unknown-freebsd` (siehe
//! `docs/setup/dod.md`). Nicht-Unix-Ziele baut der Workspace insgesamt nicht
//! (`harw-fsutil`, `harw-home` und der Spool in `harw-dod-sentinel` sind
//! Unix-only).
//!
//! # Finding spool (PL-100 WP0.12)
//! With `--findings-export`, [`run`] also opens the finding spool under
//! `<home>/spool` (module [`spool_writer`], directory `0700`, inside the
//! read-write Landlock root `--home`) **after** [`sandbox::restrict_self`],
//! so its dedicated writer thread inherits the self-restriction. Every
//! reported finding is frozen with a bounded evidence slice (its triggering
//! events plus at most 32 context events of the ring, at most 64 KiB) and
//! queued for that thread; the export line carries the record's digest
//! (`record_digest`). A full queue drops the record (counted); a spool that
//! cannot be opened leaves the sentinel running without records. Both, like
//! `SpoolFull` and `RecordTooLarge`, show up as `degraded` in the `spool`
//! object of `health.json`. Retention: 14 days and 1 GiB, aligned with the
//! `dod_spool` class of `harw-retention`.
//!
//! # Fehler
//! [`error::SentinelBinError`] — ausschließlich die Startpfade ohne
//! Degradationsoption: Root-Space-Auflösung, Öffnen des Telemetrie-Sinks
//! und die Alias-Prüfung der Schreibziele
//! ([`error::SentinelBinError::Destinations`]). Ein fehlender
//! Landlock-Support oder ein nicht bindbarer IPC-Socket sind **kein** `Err`
//! dieses Binaries (siehe oben).
//! Eine nicht vertrauenswürdige oder ungültige DoD-Konfiguration beendet den
//! Prozess ebenfalls, wird aber direkt in [`main`] als
//! [`harw_dod_config::ConfigError`] geloggt (siehe oben), weil
//! `SentinelBinError` dafür noch keine Variante trägt.
//!
//! # Examples
//! ```text
//! $ harw-sentinel --log info --home /var/lib/harwness --interval-secs 5
//! $ harw-sentinel --once --home /var/lib/harwness
//! $ harw-sentinel --print-sandbox --home /var/lib/harw-dod \
//!     --findings-export /var/lib/harw-dod/export/findings.jsonl
//! $ harw-sentinel --config /etc/harw-dod/config.toml --home /var/lib/harw-dod \
//!     --findings-export /var/lib/harw-dod/export/findings.jsonl
//! ```

#![forbid(unsafe_code)]

mod cgroup_resolve;
mod cli;
mod container_plan;
mod error;
mod export;
mod findings;
mod health_export;
mod listener_state;
mod peer_allow;
mod rates;
mod rule_config;
mod sandbox;
mod sensors;
mod spool_writer;
mod throttle;

#[cfg(target_os = "linux")]
mod ipc;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::sync::Mutex;
use std::time::Duration;

use clap::Parser as _;
use harw_dod_config::{
    Config, ConfigError, ContainerSensorMode, ResolvedObservationProfile, SYSTEM_CONFIG_PATH,
    SentinelSettings, load_config, load_system_config,
};
use harw_dod_sentinel::{Sentinel, SentinelConfig};
use harw_observe::{RoutingSink, SensorMetrics, TelemetrySink};
use harw_observe_file::FileSink;
use jiff::Timestamp;

use cli::Cli;
use error::{SentinelBinError, SentinelBinResult};
use sensors::SensorRoots;

/// Größenschwelle für die Rotation der aktiven Telemetrie-Datei.
///
/// Übernommen aus dem Aufrufbeispiel von
/// [`harw_observe_file::FileSink::open`] (`10 * 1024 * 1024`); kein
/// `--flag` dieses Binaries überschreibt sie — sollte sich das als zu
/// klein/groß erweisen, ist das eine spätere, gezielte Änderung, keine
/// Konfigurationsfläche, die dieser Knoten vorwegnehmen soll.
const TELEMETRY_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Wirksame Obergrenze rotierter Telemetrie-Dateien: `--telemetry-max-files`,
/// sonst das Klassenlimit von `telemetry_rotated` (Retention-Vorgabe; dieses
/// Binary liest keine `[retention]`-Konfiguration).
fn telemetry_max_files(flag: Option<u64>) -> Option<usize> {
    let n = flag.or_else(|| {
        harw_retention::policy_for(
            &harw_retention::RetentionConfig::default(),
            "telemetry_rotated",
        )
        .and_then(|class| class.max_files)
    })?;
    usize::try_from(n).ok()
}

/// Kapazität der IPC-Empfangs-Inbox ([`ipc::IpcInbox`]).
#[cfg(target_os = "linux")]
const IPC_INBOX_CAPACITY: usize = 256;

/// Geteiltes Handle auf die IPC-Empfangs-Inbox, wie es
/// [`BoundIpc::start_receiving`] zurückgibt und [`poll_once`]/
/// [`drain_external_events`] entgegennehmen.
///
/// Auf Linux die tatsächliche, hinter einem `Mutex` gesperrte
/// [`ipc::IpcInbox`] (siehe Moduldoku, Abschnitt „Warum `Sentinel` nicht von
/// einem `Mutex` umschlossen wird" — die Sperre sitzt hier, nicht um
/// `Sentinel`). Auf jeder anderen Plattform ein unbewohnter Platzhalter:
/// [`ipc`] wird dort gar nicht erst kompiliert, also kann dieser Typ dort
/// nie einen Wert tragen — [`bind_ipc_if_available`] liefert dort stets
/// `None`.
#[cfg(target_os = "linux")]
type IpcInboxHandle = Arc<Mutex<ipc::IpcInbox>>;

/// Siehe die Linux-Fassung.
#[cfg(not(target_os = "linux"))]
type IpcInboxHandle = ();

/// Faktor zwischen Poll-Abstand und Mindest-Rückschaufenster eines Sensors.
///
/// Zwei volle Abstände: ein Fenster von genau einem Abstand überlappt nur,
/// solange jede Runde pünktlich läuft; verspätet sich eine Runde (langsamer
/// Sensor, Last), entstünde eine Lücke. Der doppelte Abstand toleriert eine
/// Verspätung bis zu einem vollen weiteren Abstand, ohne ein Ereignis zu
/// verpassen — Doppelzählungen im Überlappungsbereich sind der billigere
/// Fehler (siehe `harw_dod_authlog::sensor`-Moduldoku, Abschnitt „woher kommt
/// `since`?").
const LOOKBACK_POLL_FACTOR: u32 = 2;

/// Der programmweite Poll-Abstand dieses Prozesses.
///
/// # Description
/// Weder `harw_dod_config::Config` noch `SentinelConfig` kennen einen
/// Poll-Abstand: `Sentinel::poll_all` hat keine eigene Uhr, sondern wird
/// von [`run`] aufgerufen, getrennt durch `std::thread::sleep(interval)`.
/// Genau dieser Schlafabstand (`--interval-secs`) ist deshalb der
/// maßgebliche Poll-Abstand, und dieser Typ ist die einzige Stelle, aus der
/// zeitgebundene Sensorfenster abgeleitet werden (siehe Moduldoku, Abschnitt
/// „DoD-Systemkonfiguration und Poll-Taktung").
///
/// # Concurrency
/// Reiner Werttyp, `Copy + Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PollTiming {
    interval: Duration,
}

impl PollTiming {
    /// Baut die Taktung aus dem über `--interval-secs` gewählten Wert.
    ///
    /// # Arguments
    /// - `secs` (`u64`): Sekunden zwischen zwei Poll-Runden.
    #[must_use]
    const fn from_interval_secs(secs: u64) -> Self {
        Self {
            interval: Duration::from_secs(secs),
        }
    }

    /// Abstand zwischen zwei Poll-Runden — exakt der Wert, den die
    /// Dauerschleife in [`run`] schläft.
    #[must_use]
    const fn interval(&self) -> Duration {
        self.interval
    }

    /// Effektives Rückschaufenster eines Sensors mit Vorgabe `default`.
    ///
    /// # Description
    /// `max(default, LOOKBACK_POLL_FACTOR × interval)` — ein Sensor mit
    /// großzügiger Vorgabe behält sie, ein zu knappes Fenster wird auf den
    /// doppelten Poll-Abstand angehoben (siehe [`LOOKBACK_POLL_FACTOR`]).
    /// Gedacht für `with_lookback`-Konstruktoren wie
    /// `harw_dod_authlog::AuthlogSensor::with_lookback`. Die Multiplikation
    /// sättigt, statt bei einem absurd großen `--interval-secs` überzulaufen.
    ///
    /// **Nicht** für Lese-Timeouts (`with_timeout` von `harw-dod-procmon`/
    /// `harw-dod-flow`): ein Timeout blockiert die Runde und muss **unter**
    /// dem Poll-Abstand bleiben, nicht darüber.
    ///
    /// # Arguments
    /// - `default` (`std::time::Duration`): die Vorgabe des Sensors.
    #[must_use]
    fn lookback_for(&self, default: Duration) -> Duration {
        default.max(self.interval.saturating_mul(LOOKBACK_POLL_FACTOR))
    }
}

/// Woher die geladenen DoD-Einstellungen stammen.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigOrigin {
    /// Ein explizit übergebener administrativer Pfad
    /// ([`harw_dod_config::load_config`]).
    Explicit(std::path::PathBuf),
    /// Der feste Systempfad [`harw_dod_config::SYSTEM_CONFIG_PATH`].
    System,
    /// Der feste Systempfad existiert nicht; eingebaute Vorgaben ohne
    /// Profil und ohne Digest.
    BuiltinDefaults,
}

impl ConfigOrigin {
    /// Der gelesene Pfad, oder `None` bei eingebauten Vorgaben.
    #[must_use]
    fn source_path(&self) -> Option<&Path> {
        match self {
            Self::Explicit(path) => Some(path),
            Self::System => Some(Path::new(SYSTEM_CONFIG_PATH)),
            Self::BuiltinDefaults => None,
        }
    }
}

/// Die für diesen Lauf aufgelösten DoD-Einstellungen.
///
/// # Description
/// Trägt das aktive Beobachtungsprofil (oder `None` im Fall
/// [`ConfigOrigin::BuiltinDefaults`]) und leitet daraus die
/// `SentinelConfig` ab. Wird in [`main`] vor jedem anderen Startschritt
/// gebaut (siehe Moduldoku).
#[derive(Debug, Clone)]
struct DodSettings {
    origin: ConfigOrigin,
    profile: Option<ResolvedObservationProfile>,
    /// `[sentinel]` der Konfiguration (PL-100); leer bei eingebauten
    /// Vorgaben. Ein Kommandozeilenflag gewinnt ([`effective_probe_peer_users`],
    /// [`effective_finding_cooldown_secs`]).
    sentinel: SentinelSettings,
}

impl DodSettings {
    /// Eingebaute Vorgaben für einen Host ohne Systemkonfiguration.
    #[must_use]
    const fn builtin_defaults() -> Self {
        Self {
            origin: ConfigOrigin::BuiltinDefaults,
            profile: None,
            sentinel: SentinelSettings::new(),
        }
    }

    /// Die aus dem DoD-Vertrag abgeleitete `SentinelConfig`.
    ///
    /// # Description
    /// Schema-Version 1 des DoD-Vertrags trägt weder Rückversuchspolitik noch
    /// Ringkapazitäten — jede Profilauflösung ergibt deshalb bewusst
    /// `SentinelConfig::default()`. Erhält der Vertrag solche Felder, ist
    /// dies die eine Stelle, an der sie übernommen werden.
    #[must_use]
    fn sentinel_config(&self) -> SentinelConfig {
        SentinelConfig::default()
    }
}

/// Lädt und löst die DoD-Konfiguration für diesen Lauf auf.
///
/// # Description
/// `explicit = Some(path)` liest über [`harw_dod_config::load_config`]
/// (jeder Fehler, auch eine fehlende Datei, ist fail closed — der
/// Administrator hat diesen Pfad ausdrücklich verlangt). `None` liest den
/// festen Systempfad über [`harw_dod_config::load_system_config`]; nur dort
/// führt eine fehlende Datei zu [`DodSettings::builtin_defaults`] mit einer
/// deutlichen Warnung. Siehe [`settings_from_loaded`].
///
/// # Errors
/// Jeder [`harw_dod_config::ConfigError`] außer einer fehlenden
/// Systemkonfiguration.
fn load_dod_settings(explicit: Option<&Path>) -> Result<DodSettings, ConfigError> {
    match explicit {
        Some(path) => settings_from_loaded(
            load_config(path),
            ConfigOrigin::Explicit(path.to_path_buf()),
        ),
        None => settings_from_loaded(load_system_config(), ConfigOrigin::System),
    }
}

/// Wandelt ein Ladeergebnis in [`DodSettings`] um — getrennt von
/// [`load_dod_settings`], damit die Fallback-/Fail-closed-Entscheidung ohne
/// Zugriff auf `/etc` testbar ist.
///
/// # Arguments
/// - `loaded` (`Result<harw_dod_config::Config, harw_dod_config::ConfigError>`):
///   das Ergebnis von `load_config`/`load_system_config`.
/// - `origin` (`ConfigOrigin`): die gelesene Quelle; nur
///   [`ConfigOrigin::System`] kennt den Fallback auf Vorgaben.
///
/// # Errors
/// Jeder Lese-, Vertrauens-, Parse- oder Auflösungsfehler, außer `NotFound`
/// beim festen Systempfad. Ein fehlendes oder unbekanntes `active_profile`
/// ist ebenfalls ein Fehler — `resolve_active` fällt nie auf ein anderes
/// Profil zurück, und dieser Prozess auch nicht.
fn settings_from_loaded(
    loaded: Result<Config, ConfigError>,
    origin: ConfigOrigin,
) -> Result<DodSettings, ConfigError> {
    match loaded {
        Ok(config) => {
            let profile = config.resolve_active()?;
            Ok(DodSettings {
                origin,
                profile: Some(profile),
                sentinel: config.sentinel().clone(),
            })
        }
        Err(ConfigError::Io(error))
            if origin == ConfigOrigin::System && error.kind() == std::io::ErrorKind::NotFound =>
        {
            tracing::warn!(
                path = SYSTEM_CONFIG_PATH,
                error = %error,
                "DoD system configuration not found; running with built-in defaults \
                 (no active profile, no config digest -- probe readiness cannot be matched)"
            );
            Ok(DodSettings::builtin_defaults())
        }
        Err(error) => Err(error),
    }
}

/// Protokolliert Herkunft, Profil und Digest der geladenen Einstellungen.
///
/// Sentinel und Sonde müssen denselben Digest melden (siehe
/// `harw_dod_config::ResolvedObservationProfile`); diese Zeile ist der
/// Sentinel-Anteil dieses Abgleichs.
fn log_dod_settings(settings: &DodSettings, timing: PollTiming) {
    let sentinel_config = settings.sentinel_config();
    match &settings.profile {
        Some(profile) => tracing::info!(
            origin = ?settings.origin,
            source = ?settings.origin.source_path(),
            profile = profile.profile_id().as_str(),
            config_digest = %profile.config_digest(),
            scope = ?profile.scope(),
            sensors = ?profile.sensors(),
            ?sentinel_config,
            poll_interval = ?timing.interval(),
            lookback_floor = ?timing.lookback_for(Duration::ZERO),
            "DoD configuration loaded"
        ),
        None => tracing::warn!(
            origin = ?settings.origin,
            ?sentinel_config,
            poll_interval = ?timing.interval(),
            lookback_floor = ?timing.lookback_for(Duration::ZERO),
            "DoD configuration absent; using built-in defaults"
        ),
    }
}

/// Woher ein wirksamer Sentinel-Wert stammt (PL-100: Flag vor
/// `[sentinel]` der Konfiguration vor eingebauter Vorgabe).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingSource {
    /// Das Kommandozeilenflag.
    CommandLine,
    /// `[sentinel]` der DoD-Konfiguration.
    DodConfig,
    /// Die eingebaute Vorgabe.
    BuiltIn,
}

impl SettingSource {
    /// Stabiler Bezeichner für Journal und `--print-sandbox`.
    const fn label(self) -> &'static str {
        match self {
            Self::CommandLine => "command-line",
            Self::DodConfig => "dod-config",
            Self::BuiltIn => "built-in",
        }
    }
}

/// Die wirksamen Sonden-Konten der `SO_PEERCRED`-Zulassung.
///
/// # Description
/// `--probe-peer-user` gewinnt, sonst `[sentinel] probe_peer_users` der
/// Konfiguration (`settings` ist `None`, wenn keine nutzbar ist, etwa bei
/// `--print-sandbox` mit ungültiger Datei), sonst
/// [`peer_allow::DEFAULT_PROBE_PEER_USERS`].
///
/// # Returns
/// Die Einträge und ihre Herkunft.
fn effective_probe_peer_users(
    cli: &Cli,
    settings: Option<&DodSettings>,
) -> (Vec<String>, SettingSource) {
    if let Some(users) = cli.probe_peer_users.as_ref() {
        return (users.clone(), SettingSource::CommandLine);
    }
    if let Some(users) = settings.and_then(|settings| settings.sentinel.probe_peer_users()) {
        return (users.to_vec(), SettingSource::DodConfig);
    }
    (
        peer_allow::DEFAULT_PROBE_PEER_USERS
            .iter()
            .map(|user| (*user).to_owned())
            .collect(),
        SettingSource::BuiltIn,
    )
}

/// Die wirksame Abklingzeit des Befund-Throttles in Sekunden.
///
/// # Description
/// `--finding-cooldown-secs` gewinnt, sonst `[sentinel]
/// finding_cooldown_secs`, sonst [`throttle::DEFAULT_COOLDOWN_SECS`]. Beide
/// Quellen sind auf höchstens 7 Tage geprüft.
///
/// # Returns
/// Die Sekunden und ihre Herkunft.
fn effective_finding_cooldown_secs(cli: &Cli, settings: &DodSettings) -> (u64, SettingSource) {
    if let Some(secs) = cli.finding_cooldown_secs {
        return (secs, SettingSource::CommandLine);
    }
    if let Some(secs) = settings.sentinel.finding_cooldown_secs() {
        return (secs, SettingSource::DodConfig);
    }
    (throttle::DEFAULT_COOLDOWN_SECS, SettingSource::BuiltIn)
}

/// Der wirksame Schalter des Container-Sensors (PL-100 WP1.8).
///
/// # Description
/// `--container-sensor` gewinnt, sonst `[sentinel] container_sensor`
/// (`settings` ist `None`, wenn keine Konfiguration nutzbar ist), sonst
/// [`ContainerSensorMode::Auto`].
///
/// # Returns
/// Den Schalter und seine Herkunft.
fn effective_container_sensor(
    cli: &Cli,
    settings: Option<&DodSettings>,
) -> (ContainerSensorMode, SettingSource) {
    if let Some(arg) = cli.container_sensor {
        return (arg.into(), SettingSource::CommandLine);
    }
    if let Some(mode) = settings.and_then(|settings| settings.sentinel.container_sensor()) {
        return (mode, SettingSource::DodConfig);
    }
    (ContainerSensorMode::Auto, SettingSource::BuiltIn)
}

/// Bestimmt die Container-Anordnungen dieses Laufs und protokolliert sie
/// (`info`: Schalter, Herkunft, Anordnungen samt Verfügbarkeitsregel;
/// `warn` je nicht unterstützter Anordnung nur mit ihrem Bezeichner — keine
/// Pfade, keine Container-Kennungen).
fn plan_container_sensors(
    cli: &Cli,
    settings: Option<&DodSettings>,
    run_root: &Path,
) -> (container_plan::ContainerPlan, SettingSource) {
    let (mode, source) = effective_container_sensor(cli, settings);
    let plan = container_plan::plan(mode, &cli.cgroup_root, run_root);
    let layouts: Vec<String> = plan
        .layouts
        .iter()
        .map(|planned| {
            format!(
                "{}:{}",
                planned.layout.label(),
                planned.availability_label()
            )
        })
        .collect();
    tracing::info!(
        mode = mode.label(),
        source = source.label(),
        layouts = ?layouts,
        "container sensor plan"
    );
    for unsupported in &plan.unsupported {
        tracing::warn!(
            layout = unsupported.label(),
            sensor = unsupported.status_sensor_id(),
            "container layout not supported: its containers are not observed"
        );
    }
    (plan, source)
}

/// Reicht je erkannter, nicht unterstützter Container-Anordnung ein
/// `SensorDegraded`-Ereignis in den Ring nach (PL-100 WP1.8, B19).
///
/// # Description
/// Läuft wie der Landlock- und der Listener-Historien-Fall vor dem ersten
/// [`poll_once`], damit die Regel `sensor-degraded` die Blindheit im
/// ersten Zyklus meldet (Befund und Exportzeile) — einmal je Start, denn
/// die Anordnung ändert sich nicht während des Laufs.
///
/// # Arguments
/// - `unsupported` (`&[container_plan::UnsupportedLayout]`): aus dem Plan.
/// - `sentinel` (`&mut Sentinel`): Ziel der Ereignisse.
/// - `now` (`Timestamp`): Zeitstempel der Ereignisse.
///
/// # Returns
/// Die Zahl der nachgereichten Ereignisse.
fn record_unsupported_container_layouts(
    unsupported: &[container_plan::UnsupportedLayout],
    sentinel: &mut Sentinel,
    now: Timestamp,
) -> usize {
    for layout in unsupported {
        sentinel.record_external_event(layout.degraded_event(now));
    }
    unsupported.len()
}

/// Einstiegspunkt.
///
/// # Description
/// Parst die Kommandozeile ohne `clap::Parser::parse` (das bei einem
/// Fehler oder `--help`/`--version` selbst `std::process::exit` aufriefe
/// und diese Funktion nie zu ihrem eigenen `ExitCode` zurückkehren ließe),
/// initialisiert `tracing`, bedient das Unterkommando `completions` bzw.
/// `--print-sandbox` ([`run_print_sandbox`]), lädt sonst die
/// DoD-Konfiguration ([`load_dod_settings`], fail closed — siehe
/// Moduldoku) und delegiert an [`run`].
///
/// # Returns
/// `ExitCode::SUCCESS` bei normalem Ende (nur nach `--once`,
/// `--print-sandbox` oder `completions`; die Dauerschleife endet sonst nur
/// durch ein Signal).
/// `ExitCode::FAILURE` bei einem CLI-Parse-Fehler, einer nicht
/// vertrauenswürdigen oder ungültigen DoD-Konfiguration oder einem
/// [`error::SentinelBinError`].
fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            if matches!(
                err.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                print!("{err}");
                return ExitCode::SUCCESS;
            }
            eprint!("{err}");
            return ExitCode::FAILURE;
        }
    };

    init_tracing(cli.log);

    // `completions` läuft vor dem Konfigurationsladen und jedem Sink-,
    // Socket- oder Landlock-Schritt: es schreibt nur ein Skript (bzw.
    // installiert es) und beendet sich — ein Host ohne DoD-Konfiguration soll
    // trotzdem Completions erzeugen können.
    let result = if cli.command.is_some() {
        run_completions_command(&cli)
    } else if cli.print_sandbox {
        // Host-Verifikation (PL-100 WP0.7): nur ausgeben, nichts binden,
        // nichts öffnen außer `O_PATH`-Deskriptoren, kein Landlock. Die
        // Wurzeln hängen nicht von der DoD-Konfiguration ab, die
        // Sonden-Konten schon (`[sentinel] probe_peer_users`): sie wird
        // deshalb gelesen; ist sie unbrauchbar, zeigt der Bericht die Werte
        // ohne sie (ein normaler Start verweigerte dann ohnehin).
        let settings = match load_dod_settings(cli.config.as_deref()) {
            Ok(settings) => Some(settings),
            Err(error) => {
                tracing::warn!(
                    "DoD configuration unusable; --print-sandbox reports without it \
                     (a normal start would refuse)"
                );
                tracing::debug!(error = %error, "DoD configuration load error");
                None
            }
        };
        run_print_sandbox(&cli, settings.as_ref(), &mut std::io::stdout().lock())
    } else {
        // `--config` wählt einen expliziten, vertrauten Pfad; ohne ihn gilt
        // ausschließlich der feste Systempfad.
        let settings = match load_dod_settings(cli.config.as_deref()) {
            Ok(settings) => settings,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    "refusing to start: DoD configuration is untrusted or invalid"
                );
                return ExitCode::FAILURE;
            }
        };
        // Die Schreibziele (Exporte und Zustandsdateien unter `--home`)
        // prüft `run` selbst, sobald `--home` aufgelöst und angelegt ist —
        // vor jedem Schreiber (fail-closed, PR #154 P1, PL-100
        // Owner-Review).
        run(cli, settings)
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(error = %err, "harw-sentinel exiting");
            ExitCode::FAILURE
        }
    }
}

/// Bedient das Unterkommando `completions`.
///
/// # Returns
/// `Ok(())` nach geschriebenem/installiertem Skript, ebenso ohne
/// `completions`-Unterkommando (No-Op).
///
/// # Errors
/// [`error::SentinelBinError::Completions`].
fn run_completions_command(cli: &Cli) -> SentinelBinResult<()> {
    let Some(harw_completions::CompletionsSubcommand::Completions(args)) = &cli.command else {
        return Ok(());
    };
    harw_completions::run_completions(
        &mut <Cli as clap::CommandFactory>::command(),
        "harw-sentinel",
        args,
        &harw_completions::HomeEnv::from_process(),
        &mut std::io::stdout().lock(),
    )?;
    Ok(())
}

/// Initialisiert den globalen `tracing`-Subscriber.
///
/// # Description
/// Muss genau einmal aufgerufen werden, als erste Anweisung nach dem
/// Parsen der Kommandozeile. Anders als `harw-cli::init_tracing` gibt es
/// hier keine interaktive Alternate-Screen-Rücksicht (dieses Binary ist ein
/// Hintergrunddienst, keine TUI) — die angeforderte Stufe wird unverändert
/// verwendet, nie stillschweigend auf `warn` reduziert.
///
/// # Arguments
/// - `level` (`cli::LogLevel`): die über `--log` angeforderte Stufe. Ein
///   ungültiger Rohwert erreicht diese Funktion nie — `clap` hat ihn bereits
///   beim Parsen abgelehnt (siehe [`cli::LogLevel`]-Moduldoku).
///
/// # Panics
/// Panics, falls bereits ein globaler Subscriber installiert ist (nur
/// erreichbar, wenn diese Funktion zweimal aufgerufen wird — ein
/// Programmierfehler).
fn init_tracing(level: cli::LogLevel) {
    use tracing_subscriber::EnvFilter;

    let filter =
        EnvFilter::try_new(level.as_filter_directive()).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

/// Führt den eigentlichen Sammelbetrieb aus.
///
/// # Description
/// Reihenfolge: Root-Space auflösen, Telemetrie-Sink öffnen (legt dabei
/// `harw_home::paths::telemetry_dir(home)` und damit auch `home` selbst
/// an, über `FileSink::open`s eigenes `create_dir_all` — diese Funktion
/// legt kein Verzeichnis ein zweites Mal an) und darüber
/// [`harw_observe::SensorMetrics`] installieren ([`install_sensor_metrics`]),
/// alle Schreibziele auf Aliase prüfen ([`check_export_destinations`],
/// fail-closed, vor jedem Schreiber), den IPC-Empfangssocket binden
/// ([`bind_ipc_if_available`], best effort — ein Fehlschlag degradiert,
/// siehe unten; noch **kein** Thread), **dann erst** über Landlock selbst
/// einschränken (nach allen Schritten, die noch neue Dateien/Sockets öffnen
/// müssen — bereits offene Deskriptoren bleiben nach einer Landlock-Regel
/// benutzbar, neue Pfade außerhalb der Regel nicht mehr), danach mit dem
/// Landlock-Beleg die IPC-Empfangsthreads starten
/// ([`BoundIpc::start_receiving`]), den Spool öffnen, Sensoren registrieren,
/// die Regelauswertung bauen ([`build_rule_cycle`]), dann entweder eine
/// einzelne Runde (`--once`) oder die Dauerschleife. Nach jeder Runde meldet
/// [`report_ipc_parse_errors`] verworfene IPC-Nachrichten.
///
/// Unmittelbar vor dem Binden löst [`peer_allow::resolve_probe_peers`] die
/// Sonden-Konten ([`effective_probe_peer_users`]) in die
/// `SO_PEERCRED`-Zulassungsliste auf (PL-100 WP0.5), und
/// [`rule_config::build`] baut die Regelkonfiguration des Hosts (WP1.6) —
/// beides vor Landlock, weil `/etc/passwd` danach nicht mehr lesbar ist.
/// Die Landlock-Wurzeln baut [`landlock_roots`] (PL-100 WP0.7: kein
/// Arbeitsverzeichnis, nie `/`); das Ergebnis der Selbstbeschränkung geht
/// als `landlock` in `health.json`. Nach dem Bau der Regelauswertung reicht
/// [`run`] eine beim Laden verworfene Listener-Historie als
/// `SensorDegraded`-Ereignis nach, ebenfalls vor dem ersten Zyklus.
///
/// Der IPC-Empfangssocket wird dabei nicht nur gebunden, sondern sein
/// Inbox-Handle auch zurückgehalten — [`poll_once`] entleert es jeden
/// Zyklus in den `Sentinel`-Puffer (siehe Moduldoku, Abschnitt „Externe
/// Ereignisse erreichen jetzt den Ringpuffer"). Degradiert die
/// Landlock-Selbstbeschränkung, wird das resultierende Ereignis lokal
/// zwischengespeichert und unmittelbar nach `Sentinel::new` — vor dem
/// ersten Poll-Zyklus — in den Puffer nachgereicht, weil zum Zeitpunkt des
/// Selbstbeschränkungsversuchs noch kein `Sentinel` existiert.
///
/// Die `SentinelConfig` stammt aus `settings` ([`DodSettings::sentinel_config`]),
/// der Schlafabstand der Dauerschleife aus [`PollTiming`] — derselbe Wert,
/// aus dem Sensor-Rückschaufenster abgeleitet werden (siehe Moduldoku,
/// Abschnitt „DoD-Systemkonfiguration und Poll-Taktung").
///
/// # Arguments
/// - `cli` (`cli::Cli`): die geparste Kommandozeile.
/// - `settings` (`DodSettings`): die bereits in [`main`] geladene und
///   aufgelöste DoD-Konfiguration.
///
/// # Errors
/// [`error::SentinelBinError::Home`], [`error::SentinelBinError::Sink`],
/// [`error::SentinelBinError::Destinations`] — siehe dortige Dokumentation.
/// Ein nicht bindbarer IPC-Socket oder eine fehlende Landlock-Unterstützung
/// sind **kein** `Err` (siehe Moduldoku).
fn run(cli: Cli, settings: DodSettings) -> SentinelBinResult<()> {
    let timing = PollTiming::from_interval_secs(cli.interval_secs);
    log_dod_settings(&settings, timing);

    let home = resolve_home(&cli)?;

    let telemetry_dir = harw_home::paths::telemetry_dir(&home);
    let file_sink: Arc<dyn TelemetrySink> = Arc::new(
        FileSink::open(&telemetry_dir, TELEMETRY_MAX_BYTES)
            .map_err(SentinelBinError::from)?
            .with_max_rotated_files(telemetry_max_files(cli.telemetry_max_files)),
    );
    tracing::info!(dir = %telemetry_dir.display(), "telemetry sink opened");
    let (sink, metrics) = install_sensor_metrics(file_sink);

    // PL-100 Owner-Review: jedes Schreibziel (Exporte und Zustandsdateien
    // unter `--home`, das `FileSink::open` eben angelegt hat) vor dem
    // ersten Schreiber auf Aliase prüfen; ein Alias verweigert den Start.
    check_export_destinations(&cli, &home)?;

    let socket_path = ipc_socket_path(&cli, &home);
    // PL-100 WP0.5: Sonden-Konten jetzt auflösen -- nach Landlock ist
    // `/etc/passwd` nicht mehr lesbar. Flag vor `[sentinel]` vor Vorgabe.
    let (peer_users, peer_source) = effective_probe_peer_users(&cli, Some(&settings));
    let peers = peer_allow::resolve_probe_peers(&peer_users, Path::new(peer_allow::PASSWD_PATH));
    log_peer_resolution(&peers, peer_source);
    // PL-100 WP1.6: Regelkonfiguration des Hosts (Home-Verzeichnisse aus
    // `/etc/passwd`, flüchtiger Portbereich, kanonische Workspace-Wurzel)
    // ebenfalls vor Landlock.
    let (host_rule_config, _rule_config_summary) = rule_config::build(
        cli.workspace_root.as_deref(),
        Path::new(peer_allow::PASSWD_PATH),
        &cli.proc_root,
    );
    // PL-100 WP1.8: Container-Laufzeiten ebenfalls vor Landlock erkennen
    // (ihre Sockets liegen unter `/run`, keiner Landlock-Wurzel).
    let (containers, _container_source) = plan_container_sensors(
        &cli,
        Some(&settings),
        Path::new(container_plan::DEFAULT_RUN_ROOT),
    );
    // Nur binden (der Pfad unter `/run/harw` ist nach Landlock nicht mehr
    // anlegbar); die Empfangsthreads starten erst mit dem Landlock-Beleg.
    let bound_ipc = bind_ipc_if_available(&socket_path);

    // Optionaler Befund-Export (`--findings-export`): vor Landlock öffnen,
    // damit ein Konfigurationsfehler sofort im Log steht; das Verzeichnis
    // bleibt danach für Rotation/Neuanlage beschreibbar (Regel unten).
    let mut exporter = build_findings_exporter(&cli, sink.as_ref());
    // Optionaler Gesundheitsexport (`--health-export`, Vorgabe: `health.json`
    // neben dem Befundexport); sein Verzeichnis braucht dieselbe Regel.
    let mut health = build_health_exporter(&cli, &settings);

    let roots = landlock_roots(
        &cli,
        &home,
        exporter
            .as_ref()
            .and_then(export::FindingsExporter::directory),
        health
            .as_ref()
            .and_then(health_export::HealthExporter::directory),
    );
    let sandbox_proof = sandbox::restrict_self(&roots);
    let outcome = sandbox_proof.outcome();
    // `Sentinel` existiert an dieser Stelle noch nicht (siehe unten) — ein
    // degradiertes Ergebnis wird deshalb zwischengespeichert, statt sofort
    // per `record_external_event` übergeben zu werden (Frage 3 der
    // Moduldoku, Abschnitt „Externe Ereignisse erreichen jetzt den
    // Ringpuffer"). Das `tracing::warn!` bleibt zusätzlich stehen (Frage 4
    // ebenda).
    let landlock_event = if outcome.is_degraded() {
        let event = sandbox::landlock_degraded_event(Timestamp::now());
        // Journal-Hygiene (PL-100 WP0.6): nie das ganze Ereignis auf `warn`.
        tracing::warn!(
            sensor = event.sensor.as_str(),
            ?outcome,
            "running without landlock self-restriction"
        );
        Some(event)
    } else {
        tracing::info!(?outcome, "landlock self-restriction applied");
        None
    };

    // PL-100 WP0.7: das Ergebnis samt ABI-Stufen steht in jedem
    // `health.json` (Host-Verifikation).
    health = health.map(|health| health.with_landlock(sandbox_proof.report()));

    // PL-100 Owner-Review: Annahme- und Empfangsthreads erst jetzt, mit dem
    // Landlock-Beleg — so erben sie die Selbstbeschränkung.
    let inbox = bound_ipc.map(|bound| bound.start_receiving(peers.allowlist, &sandbox_proof));

    // PL-100 WP0.12: Spool eingefrorener Befunde — nur mit Befundexport,
    // dessen `record_digest` auf seine Records verweist. Erst nach Landlock
    // geöffnet, damit der Schreibthread die Selbstbeschränkung erbt; der
    // Spool liegt unter `--home` (beschreibbare Landlock-Wurzel).
    let spool = exporter
        .is_some()
        .then(|| spool_writer::SpoolWriter::open(&home));
    if let Some(spool) = spool.as_ref() {
        health = health.map(|health| health.with_spool(spool.stats()));
    }

    let sensor_roots = SensorRoots {
        proc_root: cli.proc_root.clone(),
        thermal_root: cli.thermal_root.clone(),
        // PL-100 WP0.7: nur mit ausdrücklichem `--workspace-root`.
        workspace_root: cli.workspace_root.clone(),
        blockio_root: cli.blockio_root.clone(),
        gpu_root: cli.gpu_root.clone(),
        cgroup_root: cli.cgroup_root.clone(),
        home: home.clone(),
        containers: containers.layouts.clone(),
    };
    // Keiner der hier registrierten Sensoren hat heute ein Rückschaufenster
    // (`with_lookback`); sobald `harw-dod-authlog` o. Ä. hier gebaut wird,
    // erhält er `timing.lookback_for(<Vorgabe>)` — siehe `PollTiming`.
    let sensor_list = sensors::build_sensors(&sensor_roots);
    tracing::info!(
        sensor_count = sensor_list.len(),
        all_unprivileged = sensors::all_unprivileged(&sensor_list),
        "sensors registered"
    );

    let mut sentinel = Sentinel::new(sensor_list, Arc::clone(&sink), settings.sentinel_config());

    // Der zwischengespeicherte Landlock-Befund wird hier nachgereicht — vor
    // dem ersten Poll-Zyklus, direkt nachdem der `Sentinel` überhaupt zum
    // ersten Mal existiert. Damit erscheint er garantiert im allerersten
    // `freeze()`, statt bei einem verpassten ersten Zyklus verlorenzugehen.
    if let Some(event) = landlock_event {
        sentinel.record_external_event(event);
    }
    // PL-100 WP1.8: nicht unterstützte Container-Anordnungen ebenso (keine
    // stille Blindheit).
    record_unsupported_container_layouts(&containers.unsupported, &mut sentinel, Timestamp::now());

    let mut resolver = cgroup_resolve::CgroupResolver::new(cli.cgroup_root.clone());

    let mut rules = build_rule_cycle(&cli, &settings, &home, host_rule_config, spool);
    // PL-100 WP1.6: eine verworfene Listener-Historie wird nie still neu
    // gelernt (vor dem ersten Zyklus, wie der Landlock-Fall).
    record_listener_load_failure(&rules, &mut sentinel, Timestamp::now());

    loop {
        poll_once(
            &mut sentinel,
            inbox.as_ref(),
            sink.as_ref(),
            &mut rules,
            exporter.as_mut(),
            health.as_mut(),
            Some(&mut resolver),
        );
        report_ipc_parse_errors(inbox.as_ref(), metrics.as_ref());
        if cli.once {
            return Ok(());
        }
        std::thread::sleep(timing.interval());
    }
}

/// Der Root-Space dieses Laufs: `--home`, sonst `harw_home::paths::home_dir`.
///
/// # Errors
/// [`error::SentinelBinError::Home`], wenn kein Root-Space auflösbar ist.
fn resolve_home(cli: &Cli) -> SentinelBinResult<PathBuf> {
    match cli.home.clone() {
        Some(path) => Ok(path),
        None => harw_home::paths::home_dir().map_err(SentinelBinError::from),
    }
}

/// Die Zustandsdatei des Befund-Throttles unter `home` (PL-100 WP0.4).
fn throttle_state_path(home: &Path) -> PathBuf {
    home.join(throttle::DEFAULT_STATE_FILE_NAME)
}

/// Die Zustandsdatei der Listener-Historie unter `home` (PL-100 WP1.6).
fn listener_state_path(home: &Path) -> PathBuf {
    home.join(listener_state::DEFAULT_STATE_FILE_NAME)
}

/// Legt den Routing-Sink über den Telemetrie-Sink und installiert das eine
/// [`harw_observe::SensorMetrics`]-Handle dieses Prozesses (PL-100).
///
/// # Description
/// [`harw_observe::DodMetrics::install`] emittiert die Nullserie
/// `sensor_parse_errors_total` an jedes Standardziel und gibt den geteilten
/// [`harw_observe::RoutingSink`] zurück, der danach der Sink des ganzen
/// Prozesses ist (gewöhnliche Messwerte gehen unverändert an `sink`).
/// `install` flusht nicht; diese Funktion flusht danach nur das lokale Ziel
/// `sink` (den File-Sink), damit die Nullserie sofort durabel ist. Scheitert
/// die Installation (statisch ungültige Serien, praktisch unerreichbar),
/// läuft der Prozess ohne Handle weiter (gewarnt, ohne Pfad) — die
/// Erkennung darf an einer Meta-Metrik nicht hängen.
///
/// # Arguments
/// - `sink` (`Arc<dyn TelemetrySink>`): der geöffnete Telemetrie-Sink.
///
/// # Returns
/// Den Sink für alle weiteren Messwerte und das Handle (`None` nach einem
/// Installationsfehler, dann ist der Sink `sink` selbst).
fn install_sensor_metrics(
    sink: Arc<dyn TelemetrySink>,
) -> (Arc<dyn TelemetrySink>, Option<SensorMetrics>) {
    match SensorMetrics::install(RoutingSink::new(Arc::clone(&sink))) {
        Ok((routing, metrics)) => {
            sink.flush();
            tracing::info!("dod sensor meta-metrics installed (sensor_parse_errors_total)");
            let routing: Arc<dyn TelemetrySink> = routing;
            (routing, Some(metrics))
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "dod sensor meta-metrics could not be installed; running without sensor_parse_errors_total"
            );
            (sink, None)
        }
    }
}

/// Baut die Regelauswertung dieses Laufs genau so, wie [`run`] sie benutzt.
///
/// # Description
/// Netzbereich aus `egress_allow_cidrs` des aktiven Profils (PL-100 WP0.3;
/// gemeldet wird nur die Anzahl, nie die CIDRs), dazu der persistierte
/// [`throttle::FindingThrottle`] mit der wirksamen Abklingzeit
/// ([`effective_finding_cooldown_secs`]: `--finding-cooldown-secs`, sonst
/// `[sentinel] finding_cooldown_secs`, sonst die Vorgabe) und dem Zustand
/// [`throttle_state_path`] unter `home` (PL-100 WP0.4: beim Bau geladen,
/// damit Wiederholungen auch über einen Neustart hinweg nur gezählt
/// werden), die persistierte Listener-Historie [`listener_state_path`]
/// (WP1.6, ebenfalls beim Bau geladen; ein Ladefehler steht danach in
/// [`findings::RuleCycle::listener_load_failure`]), die Regelkonfiguration
/// des Hosts und, falls übergeben, der Spool-Schreiber (WP0.12). Die eine
/// Produktionsstelle für diese Komposition; die Neustart-Tests bauen über
/// sie.
///
/// # Arguments
/// - `cli` (`&Cli`): die geparste Kommandozeile.
/// - `settings` (`&DodSettings`): die aufgelöste DoD-Konfiguration.
/// - `home` (`&Path`): der Root-Space (Landlock-beschreibbar).
/// - `rule_config` (`harw_dod_rules::RuleConfig`): die vor Landlock gebaute
///   Regelkonfiguration ([`rule_config::build`]).
/// - `spool` (`Option<spool_writer::SpoolWriter>`): der nach Landlock
///   geöffnete Spool-Schreiber, nur mit Befundexport.
///
/// # Returns
/// Die [`findings::RuleCycle`] ab Zählerstand null.
fn build_rule_cycle(
    cli: &Cli,
    settings: &DodSettings,
    home: &Path,
    rule_config: harw_dod_rules::RuleConfig,
    spool: Option<spool_writer::SpoolWriter>,
) -> findings::RuleCycle {
    // PL-100 WP0.3: erlaubte Ziele aus dem aktiven Profil, nicht mehr leer.
    let network_scope = findings::network_scope_from_profile(settings.profile.as_ref());
    if network_scope.is_empty() {
        tracing::warn!(
            "egress allowlist is empty: every observed egress flow is reported as \
             outside the allowed network scope"
        );
    } else {
        tracing::info!(
            egress_allow_cidrs = network_scope.targets().count(),
            "egress allowlist loaded into the rule network scope"
        );
    }
    // PL-100 WP0.4: Wiederholungen werden je `dedup_key` entprellt; der
    // Zustand liegt im Zustandsverzeichnis (`--home`, Landlock-beschreibbar)
    // und übersteht Neustarts.
    let (cooldown_secs, cooldown_source) = effective_finding_cooldown_secs(cli, settings);
    let finding_throttle = throttle::FindingThrottle::open(
        throttle::ThrottleConfig::with_cooldown_secs(cooldown_secs),
        Some(throttle_state_path(home)),
    );
    tracing::info!(
        cooldown_secs,
        source = cooldown_source.label(),
        max_keys = throttle::MAX_KEYS,
        "finding throttle enabled"
    );
    // PL-100 WP1.6: `listener-new` gegen die persistierte Historie, alle
    // Regeln mit der Konfiguration dieses Hosts.
    let listener = listener_state::ListenerState::open(Some(listener_state_path(home)));
    let rules = findings::RuleCycle::new(network_scope)
        .with_throttle(finding_throttle)
        .with_listener_state(listener)
        .with_rule_config(rule_config);
    match spool {
        Some(spool) => rules.with_spool(spool),
        None => rules,
    }
}

/// Reicht eine beim Laden verworfene Listener-Historie als
/// `SensorDegraded`-Ereignis in den Ring nach (PL-100 WP1.6).
///
/// # Description
/// Der folgende Lernzyklus übernimmt jeden gerade offenen Listener
/// ungeprüft; das darf nie still geschehen. Wie der Landlock-Fall läuft
/// das vor dem ersten [`poll_once`], damit die Regel `sensor-degraded` es im
/// ersten Zyklus meldet (Befund und Exportzeile); `health.json` meldet
/// zusätzlich `listener_history` als `degraded`, das Journal den Grund
/// ([`listener_state::ListenerState::open`]).
///
/// # Arguments
/// - `rules` (`&findings::RuleCycle`): die eben gebaute Regelauswertung.
/// - `sentinel` (`&mut Sentinel`): Ziel des Ereignisses.
/// - `now` (`Timestamp`): Zeitstempel des Ereignisses.
///
/// # Returns
/// `true`, wenn ein Ereignis nachgereicht wurde.
fn record_listener_load_failure(
    rules: &findings::RuleCycle,
    sentinel: &mut Sentinel,
    now: Timestamp,
) -> bool {
    if rules.listener_load_failure().is_none() {
        return false;
    }
    sentinel.record_external_event(listener_state::degraded_event(now));
    true
}

/// Die Abklingzeit-Grenze der Konfiguration (`[sentinel]
/// finding_cooldown_secs`) ist dieselbe wie die von
/// `--finding-cooldown-secs` — sonst nähme die Datei einen Wert an, den das
/// Flag ablehnt.
const _: () = assert!(harw_dod_config::MAX_FINDING_COOLDOWN_SECS == throttle::MAX_COOLDOWN_SECS);

/// Baut die Landlock-Wurzeln dieses Laufs.
///
/// # Description
/// Die Sensor-Lesewurzeln und der Root-Space ([`sandbox::default_roots`];
/// die Workspace-Wurzel nur mit ausdrücklichem `--workspace-root`), dazu
/// nur lesend die Zielpräfixe der sysfs-Alias-Wurzeln
/// ([`sensors::sysfs_alias_prefixes`], PL-100 WP1.10: `/sys/devices`) und
/// lesend-schreibend das Verzeichnis des Befundexports und — falls ein
/// anderes — das des Gesundheitsexports. [`run`] und
/// [`run_print_sandbox`] teilen sich diese Funktion, damit die
/// Host-Verifikation genau die Liste zeigt, die durchgesetzt wird.
///
/// # Arguments
/// - `cli` (`&Cli`): die geparste Kommandozeile.
/// - `home` (`&Path`): der aufgelöste Root-Space.
/// - `findings_dir`, `health_dir` (`Option<&Path>`): die Exportverzeichnisse
///   (`None`, wenn der jeweilige Export aus ist).
///
/// # Returns
/// Die angeforderten [`sandbox::SandboxRoot`]s, noch ungeprüft — die
/// Prüfung (keine relative Wurzel, nie `/`) macht
/// [`sandbox::prepare_roots`].
fn landlock_roots(
    cli: &Cli,
    home: &Path,
    findings_dir: Option<&Path>,
    health_dir: Option<&Path>,
) -> Vec<sandbox::SandboxRoot> {
    let mut roots = sandbox::default_roots(
        &cli.proc_root,
        &cli.thermal_root,
        cli.workspace_root.as_deref(),
        &cli.blockio_root,
        &cli.gpu_root,
        &cli.cgroup_root,
        home,
    );
    roots.extend(
        sensors::sysfs_alias_prefixes(&[
            cli.thermal_root.as_path(),
            cli.blockio_root.as_path(),
            cli.gpu_root.as_path(),
        ])
        .into_iter()
        .map(sandbox::SandboxRoot::read_only),
    );
    if let Some(dir) = findings_dir {
        roots.push(sandbox::SandboxRoot::read_write(dir.to_path_buf()));
    }
    if let Some(dir) = health_dir
        && findings_dir != Some(dir)
    {
        roots.push(sandbox::SandboxRoot::read_write(dir.to_path_buf()));
    }
    roots
}

/// Das Verzeichnis einer Exportdatei, wie es
/// [`export::FindingsExporter::directory`] und
/// [`health_export::HealthExporter::directory`] bilden (`None` für einen
/// Pfad ohne Elternteil).
fn export_parent(path: &Path) -> Option<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Protokolliert die Startauflösung der Sonden-Konten (PL-100 WP0.5).
///
/// # Description
/// `info`: die zugelassenen uids (nur Zahlen) und die Herkunft der Liste
/// (Flag, Konfiguration oder Vorgabe). `warn` je nicht auflösbarem
/// Eintrag (der konfigurierte Kontoname eines Sonden-Kontos, kein Pfad,
/// keine Adresse): diese Sonde kann ihre Ereignisse in diesem Lauf nicht
/// einreichen.
fn log_peer_resolution(peers: &peer_allow::PeerResolution, source: SettingSource) {
    let admitted: Vec<u32> = peers.allowlist.uids().collect();
    tracing::info!(
        admitted_uids = ?admitted,
        resolved = peers.resolved.len(),
        unresolved = peers.unresolved.len(),
        source = source.label(),
        "ipc probe peer allowlist resolved"
    );
    for entry in &peers.unresolved {
        tracing::warn!(
            probe_peer_user = entry.as_str(),
            "probe peer user could not be resolved; its connections will be rejected"
        );
    }
}

/// Bedient `--print-sandbox` (PL-100 WP0.7, Host-Verifikation).
///
/// # Description
/// Löst den Root-Space und die Exportverzeichnisse genau wie [`run`] auf
/// (der Befundexport zählt nur, wenn ein `host` auflösbar ist — sonst bleibt
/// er auch in [`run`] aus), prüft die Wurzeln mit
/// [`sandbox::prepare_roots`], löst die wirksamen Sonden-Konten auf
/// ([`effective_probe_peer_users`]) und schreibt beides samt den
/// Zustandsdateien unter `--home` über [`write_sandbox_report`] nach `out`.
/// Bindet keinen Socket, öffnet keinen Sink, legt nichts an und ruft kein
/// Landlock auf.
///
/// # Arguments
/// - `cli` (`&Cli`): die geparste Kommandozeile.
/// - `settings` (`Option<&DodSettings>`): die geladene DoD-Konfiguration;
///   `None`, wenn sie unbrauchbar ist (dann gelten Flag bzw. Vorgabe).
/// - `out` (`&mut dyn std::io::Write`): Ziel des Berichts (stdout).
///
/// # Errors
/// [`error::SentinelBinError::Home`], wenn der Root-Space nicht auflösbar
/// ist; [`error::SentinelBinError::Report`] bei einem Schreibfehler.
fn run_print_sandbox(
    cli: &Cli,
    settings: Option<&DodSettings>,
    out: &mut dyn std::io::Write,
) -> SentinelBinResult<()> {
    let home = resolve_home(cli)?;
    let findings_dir = cli
        .findings_export
        .as_deref()
        .filter(|_| {
            export::resolve_host(cli.findings_host.as_ref(), Path::new(export::HOSTNAME_PATH))
                .is_some()
        })
        .and_then(export_parent);
    let health_path = cli.resolved_health_export();
    let health_dir = health_path.as_deref().and_then(export_parent);
    let roots = landlock_roots(cli, &home, findings_dir, health_dir);
    let prepared = sandbox::prepare_roots(&roots);
    let (peer_users, peer_source) = effective_probe_peer_users(cli, settings);
    let peers = peer_allow::resolve_probe_peers(&peer_users, Path::new(peer_allow::PASSWD_PATH));
    let state_files = [
        ("throttle", throttle_state_path(&home)),
        ("listener-history", listener_state_path(&home)),
    ];
    write_sandbox_report(out, &prepared, &peers, peer_source, &state_files)?;
    let (containers, container_source) =
        plan_container_sensors(cli, settings, Path::new(container_plan::DEFAULT_RUN_ROOT));
    write_container_report(out, &containers, container_source)?;
    Ok(())
}

/// Hängt die Container-Zeilen an den Bericht von `--print-sandbox` an
/// (PL-100 WP1.8): Schalter und Herkunft, je Anordnung Bezeichner,
/// Sensor-Kennung und Verfügbarkeitsregel (`on-demand`: fehlendes
/// cgroup-Verzeichnis ist eine leere Messung; `required`: beeinträchtigt),
/// je nicht unterstützter Anordnung Bezeichner und Meldekennung:
///
/// ```text
/// container-sensor	auto	built-in
/// container-layout	podman-machine-slice	container-machine	on-demand
/// container-unsupported	docker-cgroupfs	container-docker-cgroupfs
/// ```
///
/// # Errors
/// Jeder Schreibfehler von `out`.
fn write_container_report(
    out: &mut dyn std::io::Write,
    plan: &container_plan::ContainerPlan,
    source: SettingSource,
) -> std::io::Result<()> {
    writeln!(
        out,
        "container-sensor\t{}\t{}",
        plan.mode.label(),
        source.label()
    )?;
    for planned in &plan.layouts {
        writeln!(
            out,
            "container-layout\t{}\t{}\t{}",
            planned.layout.label(),
            planned.layout.sensor_id(),
            planned.availability_label()
        )?;
    }
    for unsupported in &plan.unsupported {
        writeln!(
            out,
            "container-unsupported\t{}\t{}",
            unsupported.label(),
            unsupported.status_sensor_id()
        )?;
    }
    out.flush()
}

/// Schreibt den Bericht von `--print-sandbox`.
///
/// # Description
/// Tabulatorgetrennt, eine Zeile je Eintrag; Pfade und Kontonamen in
/// Rust-`Debug`-Quotierung (Steuerzeichen escaped). `state-file` nennt die
/// Zustandsdateien unter dem beschreibbaren Root-Space (`0600`, atomar über
/// `.<name>.tmp` ersetzt; PL-100 WP0.4/WP1.6), `ipc-peer-source` die
/// Herkunft der Sonden-Konten:
///
/// ```text
/// # harw-sentinel effective sandbox (landlock abi v1, not applied by --print-sandbox)
/// landlock\tread\t"/proc"
/// landlock\tread-write\t"/var/lib/harw-dod"
/// landlock-refused\tfilesystem-root\t"/"
/// state-file\tthrottle\t"/var/lib/harw-dod/finding-throttle.json"
/// state-file\tlistener-history\t"/var/lib/harw-dod/listener-history.json"
/// ipc-peer-source\tbuilt-in
/// ipc-peer-uid\t0\talways
/// ipc-peer-uid\t991\t"harw-probe-fs"
/// ipc-peer-unresolved\t"harw-probe-netlink"
/// ```
///
/// # Errors
/// Jeder Schreibfehler von `out`.
fn write_sandbox_report(
    out: &mut dyn std::io::Write,
    prepared: &sandbox::PreparedRoots,
    peers: &peer_allow::PeerResolution,
    peer_source: SettingSource,
    state_files: &[(&str, PathBuf)],
) -> std::io::Result<()> {
    writeln!(
        out,
        "# harw-sentinel effective sandbox (landlock abi v{}, not applied by --print-sandbox)",
        sandbox::REQUESTED_ABI
    )?;
    for accepted in &prepared.accepted {
        let access = if accepted.root.writable {
            "read-write"
        } else {
            "read"
        };
        writeln!(out, "landlock\t{access}\t{:?}", accepted.root.path)?;
    }
    for (root, reason) in &prepared.refused {
        writeln!(out, "landlock-refused\t{}\t{:?}", reason.label(), root.path)?;
    }
    for (role, path) in state_files {
        writeln!(out, "state-file\t{role}\t{path:?}")?;
    }
    writeln!(out, "ipc-peer-source\t{}", peer_source.label())?;
    writeln!(out, "ipc-peer-uid\t{}\talways", peer_allow::ROOT_UID)?;
    for peer in &peers.resolved {
        writeln!(out, "ipc-peer-uid\t{}\t{:?}", peer.uid, peer.entry)?;
    }
    for entry in &peers.unresolved {
        writeln!(out, "ipc-peer-unresolved\t{entry:?}")?;
    }
    out.flush()
}

/// Führt genau eine Poll-Runde aus, loggt sie und wertet ihr Delta aus.
///
/// # Description
/// `now` wird genau hier gelesen — dies ist der Kompositionswurzel-Ort, der
/// laut `harw_dod_sentinel`-Moduldoku die Systemuhr lesen darf; jede
/// aufgerufene Bibliothekscrate bekommt `now` nur injiziert.
///
/// Reihenfolge (Frage 1 der Moduldoku, Abschnitt „Externe Ereignisse
/// erreichen jetzt den Ringpuffer"): zuerst [`Sentinel::poll_all`], **dann**
/// [`drain_external_events`] — ein zum Aufrufzeitpunkt in der Inbox
/// wartendes Ereignis landet damit nach den Sensormessungen desselben
/// Zyklus im Puffer, nicht davor. Beide Schritte laufen vor
/// [`findings::RuleCycle::evaluate`], sodass das danach eingefrorene Delta
/// immer beide Quellen dieses Zyklus enthält. `evaluate` friert nur die seit
/// dem vorigen Zyklus angekommenen Einträge ein und lässt
/// [`findings::report_findings`] darüber laufen — siehe Moduldoku, Abschnitt
/// „Regelauswertung je Zyklus über das Delta", und [`findings`]-Moduldoku
/// für die vollständige Begründung.
///
/// # Arguments
/// - `sentinel` (`&mut harw_dod_sentinel::Sentinel`): die Sammelstelle.
/// - `inbox` (`Option<&`[`IpcInboxHandle`]`>`): das Handle der
///   IPC-Empfangs-Inbox, falls der Empfangspfad auf dieser Plattform
///   existiert und der Socket gebunden werden konnte (`None` sonst — dann
///   ist dieser Poll-Zyklus rein sensor-basiert).
/// - `sink` (`&dyn harw_observe::TelemetrySink`): dasselbe Sink-Objekt, das
///   [`run`] beim Start geöffnet hat — der Ausgabeweg für gemeldete Befunde
///   (siehe [`findings`]-Moduldoku).
/// - `rules` (`&mut findings::RuleCycle`): die über Zyklen gehaltene Marke
///   der Delta-Auswertung samt erlaubtem Netzbereich und Befund-Throttle
///   (PL-100 WP0.4); jeder Aufruf rückt sie vor. Ihre Zähler verdrängter
///   Einträge gehen in `health.json` (WP0.13).
/// - `exporter` (`Option<&mut export::FindingsExporter>`): der optionale
///   JSON-Lines-Befundexport (`--findings-export`); `None` exportiert nichts.
/// - `health` (`Option<&mut health_export::HealthExporter>`): the optional
///   health document (`--health-export`); rewritten at the end of the cycle
///   from health snapshots taken around `poll_all`. `None` writes nothing
///   and takes no snapshot.
/// - `resolver` (`Option<&mut cgroup_resolve::CgroupResolver>`): maps the raw
///   cgroup ids forwarded by the probes to cgroup paths beneath
///   `--cgroup-root` (PL-97 E4); `None` leaves every received actor without
///   a cgroup. The kernel ids it resolved this cycle reach the rules as
///   [`cgroup_resolve::CgroupKids`].
///
/// # Journal
/// Sensor events are never logged with their content (paths, destinations,
/// producer text); `poll_once` logs only their count per kind on `debug`
/// (PL-100 WP0.6, see [`findings::event_kind_counts`]).
fn poll_once(
    sentinel: &mut Sentinel,
    inbox: Option<&IpcInboxHandle>,
    sink: &dyn TelemetrySink,
    rules: &mut findings::RuleCycle,
    exporter: Option<&mut export::FindingsExporter>,
    health: Option<&mut health_export::HealthExporter>,
    resolver: Option<&mut cgroup_resolve::CgroupResolver>,
) {
    let now = Timestamp::now();
    // Nur mit Gesundheitsexport: Zustand je Sensor vor dem Abruf festhalten,
    // damit `SensorTracker` Erfolg/Fehlschlag dieses Zyklus ableiten kann.
    let before = health.as_ref().map(|_| sentinel.health_snapshot());
    let reading = sentinel.poll_all(now);
    // Journal-Hygiene (PL-100 WP0.6): nie der Ereignisinhalt (Pfade, Ziele,
    // erzeugerbestimmter Text), nur Anzahl je Art und nur auf `debug`.
    tracing::debug!(
        samples = reading.samples.len(),
        events = reading.events.len(),
        event_kinds = ?findings::event_kind_counts(&reading.events),
        degraded_sensors = sentinel.degraded_count(),
        "poll cycle complete"
    );

    let cgroup_kids = drain_external_events(sentinel, inbox, resolver);

    let reported = rules.evaluate(sentinel.buffer(), sink, now, &cgroup_kids, exporter);
    tracing::debug!(
        reported,
        "security rules evaluated against this cycle's evidence delta"
    );

    if let (Some(health), Some(before)) = (health, before) {
        let after = sentinel.health_snapshot();
        // PL-100 WP1.6: Stand der Listener-Historie nach diesem Zyklus.
        health.note_listener_history(rules.listener_health());
        health.record_cycle(
            sink,
            &before,
            &after,
            ipc_counters(inbox),
            rules.ring_counters(),
            now,
        );
    }
}

/// Builds the optional health export from `--health-export` (or its
/// default next to `--findings-export`, see [`Cli::resolved_health_export`]).
///
/// # Description
/// `None` when neither flag is set. That the path does not alias the
/// findings export is checked before startup by
/// [`check_export_destinations`] (resolved identity, fail-closed). The
/// configuration identity (digest, profile) is taken from the already
/// resolved `settings`.
fn build_health_exporter(
    cli: &Cli,
    settings: &DodSettings,
) -> Option<health_export::HealthExporter> {
    let path = cli.resolved_health_export()?;
    let identity = health_export::ConfigIdentity {
        config_digest: settings
            .profile
            .as_ref()
            .map(|profile| profile.config_digest().to_string()),
        profile: settings
            .profile
            .as_ref()
            .map(|profile| profile.profile_id().as_str().to_owned()),
    };
    tracing::info!(
        path = %path.display(),
        schema_version = health_export::HEALTH_SCHEMA_VERSION,
        "health export enabled"
    );
    Some(health_export::HealthExporter::new(path, identity))
}

/// Checks every destination this process writes before any writer starts.
///
/// # Description
/// The destinations, in this order: the findings export (if configured,
/// [`export::Destination::findings`], with its rotated generations), the
/// health document (explicitly or by its default next to the findings
/// export, [`export::Destination::health`], with its temporary sibling),
/// the finding throttle state [`throttle_state_path`] (always,
/// [`export::Destination::state`], with its temporary sibling; PL-100 owner
/// review), the listener history [`listener_state_path`] (always, with
/// its temporary sibling `.listener-history.json.tmp`; PL-100 WP1.6) and
/// the IPC socket ([`ipc_socket_path`], [`export::Destination::socket`]:
/// `IpcListener::bind` unlinks it at every start, so on another writer's
/// file it would delete that file — a lost listener history would then be
/// relearned silently; PL-100 review round 1). Each path must be absolute
/// with normal components only ([`export::validate_export_path`]); the
/// state files and the default socket are checked in the canonical
/// spelling of `home` (which must exist — `run` calls this after the
/// telemetry sink created it). A socket whose directory does not exist
/// (yet) is validated but not compared: binding there fails, so nothing
/// is unlinked. Every pair is compared by resolved
/// identity — canonical parent plus file name — via
/// [`export::check_distinct_writers`], so a `.`/`..` spelling or a
/// symlinked parent cannot make an atomic rename (health document,
/// throttle state) replace the findings file while the findings writer
/// appends to the unlinked inode, nor two atomic writers overwrite each
/// other every cycle.
///
/// # Arguments
/// - `cli` (`&Cli`): the parsed command line.
/// - `home` (`&Path`): the resolved root space (`--home`).
///
/// # Errors
/// [`export::ExportPathError`] ([`export::ExportPathError::Unresolved`] also
/// when `home` cannot be canonicalised); `run` refuses to start on it
/// ([`error::SentinelBinError::Destinations`]).
fn check_export_destinations(cli: &Cli, home: &Path) -> Result<(), export::ExportPathError> {
    let state_dir =
        std::fs::canonicalize(home).map_err(|source| export::ExportPathError::Unresolved {
            parent: home.to_path_buf(),
            source,
        })?;
    let throttle_state = throttle_state_path(&state_dir);
    let listener_history = listener_state_path(&state_dir);
    let socket = ipc_socket_path(cli, &state_dir);
    let health = cli.resolved_health_export();
    let mut destinations = Vec::with_capacity(5);
    if let Some(findings) = cli.findings_export.as_deref() {
        destinations.push(export::Destination::findings(findings));
    }
    if let Some(health) = health.as_deref() {
        destinations.push(export::Destination::health(health));
    }
    destinations.push(export::Destination::state(
        "throttle state",
        &throttle_state,
    ));
    destinations.push(export::Destination::state(
        "listener history",
        &listener_history,
    ));
    export::validate_export_path("ipc socket", &socket)?;
    if socket
        .parent()
        .is_some_and(|parent| std::fs::canonicalize(parent).is_ok())
    {
        destinations.push(export::Destination::socket(&socket));
    }
    export::check_distinct_writers(&destinations)
}

/// The IPC receive socket of this run: `--socket`, else
/// `<home>/`[`cli::DEFAULT_SOCKET_FILE_NAME`].
fn ipc_socket_path(cli: &Cli, home: &Path) -> PathBuf {
    cli.socket
        .clone()
        .unwrap_or_else(|| home.join(cli::DEFAULT_SOCKET_FILE_NAME))
}

/// Reads the receive-path counters for the health export.
///
/// # Description
/// Locks the inbox once (a poisoned mutex is recovered, exactly like
/// [`drain_external_events`]); `None` (no bound socket) reports zeros.
#[cfg(target_os = "linux")]
fn ipc_counters(inbox: Option<&IpcInboxHandle>) -> health_export::IpcCounters {
    let Some(inbox) = inbox else {
        return health_export::IpcCounters::default();
    };
    let guard = match inbox.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    health_export::IpcCounters {
        probes_connected: guard.connected(),
        events_total: guard.received_total(),
        dropped_total: guard.dropped(),
        peers_rejected_total: guard.rejected_total(),
        decode_errors_total: guard.decode_errors_total(),
    }
}

/// See the Linux version; without a receive path every counter is zero.
#[cfg(not(target_os = "linux"))]
fn ipc_counters(_inbox: Option<&IpcInboxHandle>) -> health_export::IpcCounters {
    health_export::IpcCounters::default()
}

/// Baut den optionalen Befund-Export aus `--findings-export`/`--findings-host`.
///
/// # Description
/// Ohne `--findings-export` `None` — nichts wird exportiert. Mit dem Flag
/// wird das `host`-Feld über [`export::resolve_host`] bestimmt; ist es
/// weder angegeben noch aus [`export::HOSTNAME_PATH`] lesbar, bleibt der
/// Export mit einer Warnung aus (ein falscher `host` wäre für den Hub
/// wertlos). Sonst wird die Datei über
/// [`export::FindingsExporter::prepare`] vorab geöffnet; ein Fehler dabei
/// wird gezählt und geloggt, beendet den Prozess aber nie.
fn build_findings_exporter(
    cli: &Cli,
    sink: &dyn TelemetrySink,
) -> Option<export::FindingsExporter> {
    let path = cli.findings_export.clone()?;
    let Some(host) =
        export::resolve_host(cli.findings_host.as_ref(), Path::new(export::HOSTNAME_PATH))
    else {
        tracing::warn!(
            path = %path.display(),
            "findings export disabled: no --findings-host and the kernel hostname is unavailable"
        );
        return None;
    };
    let mut exporter =
        export::FindingsExporter::new(path, host).with_keep(cli.findings_export_keep);
    exporter.prepare(sink);
    tracing::info!(
        path = %exporter.path().display(),
        max_bytes = export::MAX_EXPORT_BYTES,
        keep = cli.findings_export_keep,
        "findings export enabled"
    );
    Some(exporter)
}

/// Entleert die IPC-Empfangs-Inbox in [`Sentinel::buffer`], über den
/// zweiten Schreibweg [`Sentinel::record_external_event`].
///
/// # Description
/// Sperrt `inbox` genau einmal, entleert sie **innerhalb dieser einen
/// Sperrspanne** in ein lokales `Vec` (über [`ipc::IpcInbox::drain`]) und
/// gibt die Sperre danach sofort wieder frei, bevor `sentinel` überhaupt
/// berührt wird — siehe Moduldoku, Abschnitt „Warum `Sentinel` nicht von
/// einem `Mutex` umschlossen wird", für die Begründung, warum die Sperre
/// hier endet, statt auf `Sentinel` ausgedehnt zu werden. Ein vergifteter
/// Mutex (nur erreichbar, wenn ein Empfangsthread mit gehaltener Sperre
/// paniert) wird wiederhergestellt statt die wartenden Ereignisse
/// stillschweigend zu verwerfen — der zugrunde liegende `VecDeque`-Zustand
/// bleibt in diesem Fall unversehrt, nur die Panik-Herkunft ist unbekannt.
///
/// `inbox == None` (kein Empfangspfad auf dieser Plattform, oder das Binden
/// des Sockets ist beim Start gescheitert) ist ein No-Op, kein Fehler.
///
/// # Arguments
/// - `sentinel` (`&mut harw_dod_sentinel::Sentinel`): Ziel jedes entleerten
///   Ereignisses.
/// - `inbox` (`Option<&`[`IpcInboxHandle`]`>`): siehe [`poll_once`].
/// - `resolver` (`Option<&mut cgroup_resolve::CgroupResolver>`): siehe
///   [`poll_once`].
///
/// # Returns
/// Die Kernel-IDs der in diesem Aufruf aufgelösten Cgroup-Pfade
/// ([`cgroup_resolve::CgroupKids`]); [`findings::RuleCycle::evaluate`]
/// braucht sie für `cgroup_kid` und den `dedup_key` (PL-100 WP0.4). Leer,
/// wenn nichts ankam.
///
/// # Concurrency
/// Läuft ausschließlich aus dem Thread, der auch [`poll_once`]/`poll_all`
/// aufruft (siehe Moduldoku). Der einzige gesperrte Abschnitt ist der
/// `inbox.lock()`-Aufruf selbst; `sentinel.record_external_event` läuft
/// danach lock-frei.
#[cfg(target_os = "linux")]
fn drain_external_events(
    sentinel: &mut Sentinel,
    inbox: Option<&IpcInboxHandle>,
    mut resolver: Option<&mut cgroup_resolve::CgroupResolver>,
) -> cgroup_resolve::CgroupKids {
    let mut cgroup_kids = cgroup_resolve::CgroupKids::default();
    let Some(inbox) = inbox else {
        return cgroup_kids;
    };

    let received = {
        let mut guard = match inbox.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::warn!(
                    "ipc inbox mutex poisoned by a panicked thread; recovering its contents"
                );
                poisoned.into_inner()
            }
        };
        // Nur Verdrängungen seit dem vorigen Zyklus: `dropped()` wächst
        // monoton, die Warnung unten soll je Verlust einmal kommen, nicht in
        // jedem späteren Zyklus erneut.
        let dropped = guard.take_newly_dropped();
        // Der häufigste Fall: nichts angekommen. Dann gar nicht erst
        // entleeren -- `drain()` legt sonst je Zyklus einen leeren `Vec` an.
        if guard.is_empty() && dropped == 0 {
            return cgroup_kids;
        }
        // Auslastung ablesen, solange die Sperre noch gehalten wird --
        // danach ist die Inbox leer.
        let held = guard.len();
        let capacity = guard.capacity();
        let dropped_total = guard.dropped();
        let drained = guard.drain();
        (drained, held, capacity, dropped, dropped_total)
    };
    let (received, held, capacity, dropped, dropped_total) = received;

    if !received.is_empty() {
        tracing::debug!(
            held,
            capacity,
            "draining externally received events from the ipc inbox"
        );
    }
    // Erwartet wird null. Jede Verdrängung heißt, dass eine privilegierte
    // Sonde etwas beobachtet hat, das nie im Ringpuffer ankam -- und ein
    // `freeze()` danach ist unvollständig, ohne dass irgendetwas rot wird.
    if dropped > 0 {
        tracing::warn!(
            dropped,
            dropped_total,
            capacity,
            "ipc inbox overflowed; observations from a privileged probe were lost"
        );
    }

    if let Some(resolver) = resolver.as_deref_mut() {
        resolver.begin_batch();
    }
    for mut item in received {
        // Rohe cgroup-ID der Sonde -> Pfad unter `--cgroup-root`, oder
        // keine cgroup (PL-97 E4, siehe `cgroup_resolve`).
        // Die Kernel-ID bleibt für diesen Zyklus neben dem Pfad erhalten
        // (`cgroup_kid` der Befunde, PL-100 WP0.4/WP0.9).
        if let Some(actor) = item.event.actor.as_mut() {
            let kid = cgroup_resolve::resolve_actor_cgroup(actor, resolver.as_deref_mut());
            if let (Some(kid), Some(path)) = (kid, actor.cgroup.as_ref()) {
                cgroup_kids.record(path.as_str(), kid);
            }
        }
        tracing::debug!(
            sensor = %item.event.sensor.as_str(),
            peer_pid = item.peer.pid,
            "recording externally received security event into the evidence buffer"
        );
        sentinel.record_external_event(item.event);
    }
    cgroup_kids
}

/// Siehe die Linux-Fassung. `inbox` ist auf dieser Plattform stets `None`
/// (siehe [`IpcInboxHandle`], [`bind_ipc_if_available`]) — diese Funktion
/// ist entsprechend ein reines No-Op und liefert keine Kernel-IDs.
#[cfg(not(target_os = "linux"))]
fn drain_external_events(
    _sentinel: &mut Sentinel,
    _inbox: Option<&IpcInboxHandle>,
    _resolver: Option<&mut cgroup_resolve::CgroupResolver>,
) -> cgroup_resolve::CgroupKids {
    cgroup_resolve::CgroupKids::default()
}

/// Der gebundene, noch nicht empfangende IPC-Socket samt seiner Inbox
/// (PL-100 Owner-Review, siehe Moduldoku, Abschnitt „Binden vor Landlock,
/// Empfangen danach").
///
/// # Description
/// Entsteht in [`bind_ipc_if_available`] vor der Landlock-Selbstbeschränkung
/// und startet **keinen** Thread. Erst [`BoundIpc::start_receiving`] startet
/// Annahme- und Empfangsthreads; es verlangt den
/// [`sandbox::SandboxApplied`]-Beleg, den nur [`sandbox::restrict_self`]
/// ausstellt.
///
/// # Concurrency
/// Lebt im Startthread, bis es in [`BoundIpc::start_receiving`] verbraucht
/// wird.
#[cfg(target_os = "linux")]
#[derive(Debug)]
struct BoundIpc {
    // Der lauschende Socket; noch nimmt niemand an.
    listener: ipc::IpcListener,
    // Die Inbox, die die Empfangsthreads später füllen.
    inbox: IpcInboxHandle,
}

#[cfg(target_os = "linux")]
impl BoundIpc {
    /// Startet die Annahmeschleife (und darüber je zugelassener Verbindung
    /// einen Empfangsthread) — erst nach der Landlock-Selbstbeschränkung.
    ///
    /// # Arguments
    /// - `allowlist` (`peer_allow::PeerAllowlist`): die vor Landlock
    ///   aufgelöste `SO_PEERCRED`-Zulassung (PL-100 WP0.5).
    /// - `proof` (`&sandbox::SandboxApplied`): der Beleg, dass
    ///   [`sandbox::restrict_self`] bereits lief; die neuen Threads erben
    ///   damit die Selbstbeschränkung (auch ein degradiertes Ergebnis ist
    ///   ein Beleg — die Reihenfolge gilt immer).
    ///
    /// # Returns
    /// Das Inbox-Handle für [`poll_once`]. Bis zu diesem Aufruf wartende
    /// Verbindungen liegen im Backlog des Sockets und werden jetzt
    /// angenommen.
    ///
    /// # Concurrency
    /// Startet `std::thread::spawn` (siehe [`ipc::spawn_receive_loop`]).
    fn start_receiving(
        self,
        allowlist: peer_allow::PeerAllowlist,
        proof: &sandbox::SandboxApplied,
    ) -> IpcInboxHandle {
        tracing::debug!(
            landlock = ?proof.outcome(),
            "starting the ipc receive threads after the landlock self-restriction"
        );
        let _accept_loop =
            ipc::spawn_receive_loop(self.listener, Arc::clone(&self.inbox), allowlist);
        self.inbox
    }
}

/// Siehe die Linux-Fassung: ohne Empfangspfad unbewohnt, es gibt nie einen
/// gebundenen Socket.
#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
enum BoundIpc {}

#[cfg(not(target_os = "linux"))]
impl BoundIpc {
    /// Siehe die Linux-Fassung; unerreichbar, weil es keinen Wert gibt.
    fn start_receiving(
        self,
        _allowlist: peer_allow::PeerAllowlist,
        _proof: &sandbox::SandboxApplied,
    ) -> IpcInboxHandle {
        match self {}
    }
}

/// Bindet den IPC-Empfangssocket, falls möglich — **ohne** einen Thread zu
/// starten.
///
/// # Description
/// Läuft vor [`sandbox::restrict_self`], weil der Socketpfad (etwa unter
/// `/run/harw`) keine Landlock-Wurzel ist. Best effort: ein Fehlschlag wird
/// geloggt, degradiert diesen Prozess aber nicht — die Sammelschleife läuft
/// unverändert weiter, nur ohne externe Ereignisse aus dieser Quelle.
/// Annahme und Empfang startet erst [`BoundIpc::start_receiving`].
///
/// # Arguments
/// - `socket_path` (`&std::path::Path`): Zielpfad des Sockets
///   (`--socket` oder `<home>/`[`cli::DEFAULT_SOCKET_FILE_NAME`]).
///
/// # Returns
/// `Some(bound)`, wenn der Socket gebunden wurde; `None`, wenn das Binden
/// scheitert. Auf jeder Nicht-Linux-Plattform stets `None` mit einer
/// entsprechenden Logmeldung, weil [`ipc`] dort gar nicht erst kompiliert
/// wird (siehe [`ipc`]-Moduldoku).
#[cfg(target_os = "linux")]
fn bind_ipc_if_available(socket_path: &std::path::Path) -> Option<BoundIpc> {
    match ipc::IpcListener::bind(socket_path) {
        Ok(listener) => {
            let inbox: IpcInboxHandle =
                Arc::new(Mutex::new(ipc::IpcInbox::new(IPC_INBOX_CAPACITY)));
            // Der Pfad kommt vom Listener, nicht aus `socket_path`: gemeldet
            // wird damit, woran tatsächlich gebunden wurde, nicht was
            // angefordert war. Die beiden können auseinandergehen, und dann
            // ist die maßgebliche Angabe die des Listeners.
            tracing::info!(path = %listener.path().display(), "ipc receive socket bound");
            Some(BoundIpc { listener, inbox })
        }
        Err(error) => {
            // Journal-Hygiene: auf `warn` kein Pfad (auch nicht im
            // Fehlertext), nur ob ein fremder Eintrag den Pfad belegt.
            tracing::warn!(
                path_occupied = matches!(error, ipc::IpcError::NotASocket { .. }),
                "ipc receive socket unavailable; probes cannot reach this sentinel this run"
            );
            tracing::debug!(
                error = %error,
                path = %socket_path.display(),
                "ipc socket bind error"
            );
            None
        }
    }
}

/// Siehe die Linux-Fassung. `harw-sentinel` läuft nur auf Linux vollständig
/// (Landlock, `SOCK_SEQPACKET`/`SO_PEERCRED`, alle Sensor-Quellen); auf
/// jeder anderen Plattform bleibt der IPC-Empfangspfad ungebunden und diese
/// Funktion gibt stets `None` zurück.
#[cfg(not(target_os = "linux"))]
fn bind_ipc_if_available(socket_path: &std::path::Path) -> Option<BoundIpc> {
    tracing::warn!(
        path = %socket_path.display(),
        "ipc receive path is only implemented on linux; running without it"
    );
    None
}

/// Meldet die seit dem vorigen Zyklus verworfenen IPC-Nachrichten
/// (PL-100: `sensor_parse_errors_total`).
///
/// # Description
/// Sperrt die Inbox einmal (eine vergiftete Sperre wird übernommen, wie in
/// [`drain_external_events`]) und nimmt die neuen Verwürfe
/// ([`ipc::IpcInbox::take_new_decode_errors`]: nicht dekodierbare oder zu
/// große Nachrichten zugelassener Sonden). Ist es mindestens einer, geht der
/// Block **einmal** an `metrics`
/// ([`harw_observe::SensorMetrics::sensor_parse_errors`]) und auf `warn` —
/// nur mit der Anzahl, nie mit Nutzlast, Peer oder Fehlertext (die stehen
/// je Nachricht auf `debug` im Empfangsthread). Eine flutende Sonde erzeugt
/// so höchstens eine Zeile und eine Zustellung je Zyklus.
///
/// # Arguments
/// - `inbox` (`Option<&IpcInboxHandle>`): siehe [`poll_once`]; `None`
///   meldet nichts.
/// - `metrics` (`Option<&SensorMetrics>`): das installierte Handle
///   ([`install_sensor_metrics`]); `None` zählt nur im Journal.
///
/// # Returns
/// Die Anzahl der in diesem Aufruf gemeldeten Verwürfe.
#[cfg(target_os = "linux")]
fn report_ipc_parse_errors(inbox: Option<&IpcInboxHandle>, metrics: Option<&SensorMetrics>) -> u64 {
    let Some(inbox) = inbox else {
        return 0;
    };
    let newly = match inbox.lock() {
        Ok(mut guard) => guard.take_new_decode_errors(),
        Err(poisoned) => poisoned.into_inner().take_new_decode_errors(),
    };
    if newly > 0 {
        tracing::warn!(
            discarded = newly,
            "probes sent ipc messages that could not be decoded or were too large; discarded and counted"
        );
        if let Some(metrics) = metrics {
            metrics.sensor_parse_errors(newly);
        }
    }
    newly
}

/// Siehe die Linux-Fassung; ohne Empfangspfad gibt es nichts zu melden.
#[cfg(not(target_os = "linux"))]
fn report_ipc_parse_errors(
    _inbox: Option<&IpcInboxHandle>,
    _metrics: Option<&SensorMetrics>,
) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    // Eine Laufzeit-Zusicherung über eine Konstante kann nicht fehlschlagen --
    // clippy hat recht, dass das kein Test ist. Als `const`-Zusicherung wird
    // daraus eine echte Invariante: sie scheitert beim Übersetzen, wenn
    // jemand die Konstante auf null setzt.
    const _: () = assert!(TELEMETRY_MAX_BYTES > 0);

    /// Eine Regelauswertung ab Zählerstand null mit leerem Netzbereich.
    fn rule_cycle() -> findings::RuleCycle {
        findings::RuleCycle::new(harw_authority::NetworkScope::empty())
    }

    /// Flut-Regressionstest über den echten Poll-Zyklus (PL-100 WP0.1): ein
    /// Ereignis, das über viele Zyklen im Ring liegt, wird genau einmal
    /// exportiert.
    #[test]
    fn test_poll_once_exports_a_ring_event_once_across_many_cycles() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = export::FindingsExporter::new(path, host);
        let mut sentinel = Sentinel::new(
            Vec::new(),
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        sentinel.record_external_event(harw_dod_signals::SecurityEvent {
            sensor: harw_types::SensorId::from_str("probe-bpf-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: harw_dod_signals::EventKind::EgressFlow {
                destination: "198.51.100.5".to_owned(),
                port: 443,
            },
        });
        let mut rules = rule_cycle();

        for _ in 0..12 {
            poll_once(
                &mut sentinel,
                None,
                &harw_observe::NullSink,
                &mut rules,
                Some(&mut exporter),
                None,
                None,
            );
        }

        assert_eq!(
            sentinel.buffer().event_len(),
            1,
            "the ring keeps the event as context"
        );
        assert_eq!(
            exporter.written(),
            1,
            "twelve cycles over the same ring export exactly one finding"
        );
        Ok(())
    }

    /// Ereigniszeit der Kompositions-Fixtures unten (fest, nie die Uhr).
    const FIXTURE_OBSERVED_SECS: i64 = 1_700_000_000;

    /// Kernel-ID der rohen Cgroup der Fixtures (nie null, siehe
    /// `Actor::raw_cgroup`).
    const FIXTURE_CGROUP_KID: u64 = 4_242;

    /// Derselbe ausgehende Fluss bei jedem Start der Fixtures: Ziel außerhalb
    /// des (leeren) erlaubten Netzbereichs, Akteur mit uid und roher Cgroup.
    fn fixture_egress_event() -> TestResult<harw_dod_signals::SecurityEvent> {
        Ok(harw_dod_signals::SecurityEvent {
            sensor: harw_types::SensorId::from_str("probe-bpf-0"),
            observed_at: Timestamp::from_second(FIXTURE_OBSERVED_SECS)
                .map_err(ctx("fixture timestamp"))?,
            actor: Some(harw_dod_signals::Actor {
                uid: 1000,
                auid: None,
                cgroup: harw_dod_signals::Actor::raw_cgroup(FIXTURE_CGROUP_KID),
                process: None,
            }),
            kind: harw_dod_signals::EventKind::EgressFlow {
                destination: "198.51.100.5".to_owned(),
                port: 443,
            },
        })
    }

    /// Ein „Prozessstart" über die Produktionsbauer: Befundexport aus der
    /// Kommandozeile ([`build_findings_exporter`]), Regelauswertung samt
    /// persistiertem Throttle unter `--home` ([`build_rule_cycle`]), ein
    /// frischer `Sentinel` mit `event` im Ring und genau ein [`poll_once`].
    /// Beim Verlassen enden Regelauswertung und Exporter wie beim
    /// Prozessende.
    fn start_once(
        cli: &Cli,
        home: &Path,
        event: harw_dod_signals::SecurityEvent,
    ) -> TestResult<export::FindingsExporter> {
        start_with(
            cli,
            &DodSettings::builtin_defaults(),
            home,
            vec![event],
            None,
        )
    }

    /// Wie [`start_once`] mit beliebigen Ereignissen, eigener
    /// Konfiguration und optionalem Gesundheitsexport; reicht wie [`run`]
    /// eine verworfene Listener-Historie vor dem Zyklus nach
    /// ([`record_listener_load_failure`]).
    fn start_with(
        cli: &Cli,
        settings: &DodSettings,
        home: &Path,
        events: Vec<harw_dod_signals::SecurityEvent>,
        health: Option<&mut health_export::HealthExporter>,
    ) -> TestResult<export::FindingsExporter> {
        let mut exporter = build_findings_exporter(cli, &harw_observe::NullSink)
            .ok_or(TestError::Missing("findings exporter"))?;
        let mut rules =
            build_rule_cycle(cli, settings, home, harw_dod_rules::RuleConfig::new(), None);
        let mut sentinel = Sentinel::new(
            Vec::new(),
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        record_listener_load_failure(&rules, &mut sentinel, Timestamp::UNIX_EPOCH);
        for event in events {
            sentinel.record_external_event(event);
        }
        poll_once(
            &mut sentinel,
            None,
            &harw_observe::NullSink,
            &mut rules,
            Some(&mut exporter),
            health,
            None,
        );
        Ok(exporter)
    }

    /// Die geparsten Zeilen des Befundexports, gelesen mit `harw-dod-client`.
    fn client_lines(path: &Path) -> TestResult<Vec<harw_dod_client::findings::FindingLine>> {
        let text = std::fs::read_to_string(path).map_err(ctx("read findings export"))?;
        text.lines()
            .map(|line| {
                harw_dod_client::findings::parse_finding_line(line)
                    .ok_or(TestError::Missing("a line harw-dod-client accepts"))
            })
            .collect()
    }

    /// Die `occurrences` des einzigen Schlüssels der Throttle-Zustandsdatei.
    fn stored_occurrences(home: &Path) -> TestResult<u64> {
        let text = std::fs::read_to_string(throttle_state_path(home))
            .map_err(ctx("read throttle state"))?;
        let value: serde_json::Value =
            serde_json::from_str(&text).map_err(ctx("parse throttle state"))?;
        let entries = value
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .ok_or(TestError::Missing("state entries"))?;
        assert_eq!(entries.len(), 1, "one dedup key: {text}");
        entries
            .first()
            .and_then(|entry| entry.get("occurrences"))
            .and_then(serde_json::Value::as_u64)
            .ok_or(TestError::Missing("stored occurrences"))
    }

    /// PL-100 owner review (4504896419/4504912166), composition level: the
    /// production builder persists the throttle under `--home`; a restart
    /// reloads the state file, the next identical finding is suppressed
    /// (nothing exported) while its occurrence count survives, and a later
    /// report carries the count accumulated across both restarts.
    #[test]
    fn test_restart_reloads_the_throttle_state_and_suppresses_the_identical_finding() -> TestResult
    {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let export_dir = home.path().join("export");
        std::fs::create_dir(&export_dir).map_err(ctx("export dir"))?;
        let findings = export_dir.join("findings.jsonl");
        let args = |cooldown: &'static str| -> TestResult<Cli> {
            Cli::try_parse_from([
                "harw-sentinel",
                "--home",
                utf8(home.path())?,
                "--findings-export",
                utf8(&findings)?,
                "--findings-host",
                "host-a",
                "--finding-cooldown-secs",
                cooldown,
            ])
            .map_err(ctx("valid cli"))
        };

        // Start 1: neuer Schlüssel, gemeldet und persistiert.
        let first = start_once(&args("600")?, home.path(), fixture_egress_event()?)?;
        assert_eq!(first.written(), 1, "the first finding is reported");
        assert_eq!(stored_occurrences(home.path())?, 1);

        // Start 2: derselbe Befund innerhalb der Abklingzeit — unterdrückt,
        // aber gezählt und wieder persistiert.
        let second = start_once(&args("600")?, home.path(), fixture_egress_event()?)?;
        assert_eq!(
            second.written(),
            0,
            "the identical finding is suppressed after the restart"
        );
        assert_eq!(
            stored_occurrences(home.path())?,
            2,
            "the reloaded count is kept and incremented"
        );

        // Start 3 ohne Abklingzeit: gemeldet, mit dem über beide Neustarts
        // gezählten Stand und der ersten Sichtung von Start 1.
        let third = start_once(&args("0")?, home.path(), fixture_egress_event()?)?;
        assert_eq!(third.written(), 1);
        let lines = client_lines(&findings)?;
        assert_eq!(lines.len(), 2, "starts 1 and 3 wrote one line each");
        let (Some(reported), Some(repeat)) = (lines.first(), lines.last()) else {
            return Err(TestError::Missing("two export lines"));
        };
        assert_eq!(reported.occurrences, Some(1));
        assert_eq!(repeat.occurrences, Some(3));
        assert!(reported.dedup_key.is_some());
        assert_eq!(repeat.dedup_key, reported.dedup_key, "same dedup key");
        assert_eq!(repeat.first_seen, reported.first_seen, "first seen kept");
        assert_eq!(
            repeat.first_seen.map(|at| at.unix_timestamp()),
            Some(FIXTURE_OBSERVED_SECS)
        );
        assert_ne!(repeat.instance, reported.instance, "a new process instance");
        assert_eq!(repeat.cgroup_kid, Some(FIXTURE_CGROUP_KID));
        Ok(())
    }

    /// PL-100 owner review (4504896423): the real call site (`poll_once` →
    /// `report_findings` → spool and exporter) writes the canonical v3
    /// envelope, and `harw-dod-client` reads one sentinel line back with the
    /// digest of the spooled record and the same identifiers. (Building the
    /// triage context from it is `harw-ops`' half, outside this crate.)
    #[test]
    fn test_exported_v3_line_reads_back_through_harw_dod_client_with_its_record() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let export_dir = home.path().join("export");
        std::fs::create_dir(&export_dir).map_err(ctx("export dir"))?;
        let findings = export_dir.join("findings.jsonl");
        let cli = Cli::try_parse_from([
            "harw-sentinel",
            "--home",
            utf8(home.path())?,
            "--findings-export",
            utf8(&findings)?,
            "--findings-host",
            "host-a",
        ])
        .map_err(ctx("valid cli"))?;
        let mut exporter = build_findings_exporter(&cli, &harw_observe::NullSink)
            .ok_or(TestError::Missing("findings exporter"))?;
        let spool = spool_writer::SpoolWriter::open(home.path());
        assert!(spool.is_available(), "the spool opens under --home");
        let mut rules = build_rule_cycle(
            &cli,
            &DodSettings::builtin_defaults(),
            home.path(),
            harw_dod_rules::RuleConfig::new(),
            Some(spool),
        );
        let mut sentinel = Sentinel::new(
            Vec::new(),
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        sentinel.record_external_event(fixture_egress_event()?);

        poll_once(
            &mut sentinel,
            None,
            &harw_observe::NullSink,
            &mut rules,
            Some(&mut exporter),
            None,
            None,
        );
        // Ende der Regelauswertung: der Spool-Schreiber schreibt alles
        // Eingereihte, bevor sein Drop zurückkehrt.
        drop(rules);

        let text = std::fs::read_to_string(&findings).map_err(ctx("read findings export"))?;
        let line = text.lines().next().ok_or(TestError::Missing("one line"))?;
        let raw: serde_json::Value = serde_json::from_str(line).map_err(ctx("json line"))?;
        assert_eq!(
            raw.get("schema_version")
                .and_then(serde_json::Value::as_u64),
            Some(u64::from(export::EXPORT_SCHEMA_VERSION))
        );
        for field in [
            "record_digest",
            "instance",
            "seq",
            "dedup_key",
            "occurrences",
            "first_seen",
            "kind",
            "hardness",
            "cgroup_kid",
        ] {
            assert!(raw.get(field).is_some(), "v3 field {field} missing: {line}");
        }

        let parsed = harw_dod_client::findings::parse_finding_line(line)
            .ok_or(TestError::Missing("a line harw-dod-client accepts"))?;
        let spool_dir = home.path().join(spool_writer::SPOOL_DIR_NAME);
        let reader = harw_dod_sentinel::spool::FindingSpool::open(&spool_dir)
            .map_err(ctx("open the spool"))?;
        let (entries, _) = reader.page(None, 8).map_err(ctx("spool page"))?;
        assert_eq!(entries.len(), 1, "one record for one finding");
        let entry = entries
            .into_iter()
            .next()
            .ok_or(TestError::Missing("spool entry"))?;
        let record = entry.record.map_err(ctx("spooled record"))?;

        let digest = record.digest().to_string();
        assert_eq!(entry.id.digest(), record.digest());
        assert_eq!(parsed.record_digest.as_deref(), Some(digest.as_str()));
        assert_eq!(
            raw.get("record_digest").and_then(serde_json::Value::as_str),
            Some(digest.as_str())
        );
        assert_eq!(&parsed.finding_id, record.finding_id());
        assert_eq!(parsed.instance.as_deref(), Some(exporter.instance()));
        assert_eq!(parsed.seq, Some(1));
        assert_eq!(
            parsed.dedup_key.as_deref(),
            raw.get("dedup_key").and_then(serde_json::Value::as_str)
        );
        assert_eq!(parsed.occurrences, Some(1));
        assert_eq!(
            parsed.first_seen.map(|at| at.unix_timestamp()),
            Some(FIXTURE_OBSERVED_SECS)
        );
        assert_eq!(
            parsed.kind,
            Some(harw_dod_client::findings::FindingKind::RuleTriggered)
        );
        assert_eq!(
            parsed.hardness,
            Some(harw_dod_client::findings::FindingHardness::Observed)
        );
        assert_eq!(parsed.cgroup_kid, Some(FIXTURE_CGROUP_KID));
        assert_eq!(parsed.rule_id, "egress-flow");
        assert_eq!(parsed.host.as_str(), "host-a");
        Ok(())
    }

    #[test]
    fn test_run_rejects_a_home_override_pointing_at_a_file_not_a_directory() -> TestResult {
        let file = tempfile::NamedTempFile::new().map_err(ctx("temp file"))?;
        let path = file
            .path()
            .to_str()
            .ok_or(TestError::Missing("utf8 path"))?;
        let cli = Cli::try_parse_from(["harw-sentinel", "--once", "--home", path])
            .map_err(ctx("valid cli"))?;

        let result = run(cli, DodSettings::builtin_defaults());
        assert!(
            matches!(
                result,
                Err(SentinelBinError::Sink(_)) | Err(SentinelBinError::Home(_))
            ),
            "a non-directory --home must fail to resolve or to open the telemetry sink"
        );
        Ok(())
    }

    #[test]
    fn test_landlock_degraded_event_survives_being_captured_before_the_sentinel_exists()
    -> TestResult {
        // Spiegelt die Reihenfolge in `run()`: `sandbox::restrict_self` (und
        // damit `landlock_degraded_event`) läuft, bevor `Sentinel::new` je
        // aufgerufen wird — die Antwort auf Frage 3 der Moduldoku, Abschnitt
        // „Externe Ereignisse erreichen jetzt den Ringpuffer": das Ereignis
        // wird zwischengespeichert und erst nach der Konstruktion nachgereicht.
        let landlock_event = Some(sandbox::landlock_degraded_event(Timestamp::UNIX_EPOCH));

        // An dieser Stelle existiert `sentinel` in `run()` noch nicht.
        let mut sentinel = Sentinel::new(
            Vec::new(),
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        if let Some(event) = landlock_event {
            sentinel.record_external_event(event);
        }

        assert_eq!(
            sentinel.buffer().event_len(),
            1,
            "the landlock event must not be lost"
        );
        let evidence = sentinel
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        assert_eq!(evidence.events.len(), 1);
        assert_eq!(
            evidence.events[0].sensor.as_str(),
            sandbox::LANDLOCK_STATUS_SENSOR_ID
        );
        Ok(())
    }

    const HOST_PROFILE: &str = r#"
schema_version = 1
mode = "observe"
active_profile = "host"

[profiles.host]
scope = "host"
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []
"#;

    fn not_found() -> ConfigError {
        ConfigError::Io(std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    #[test]
    fn test_lookback_keeps_a_generous_default_and_raises_a_tight_one() {
        let default = Duration::from_secs(300);
        assert_eq!(
            PollTiming::from_interval_secs(5).lookback_for(default),
            default
        );
        assert_eq!(
            PollTiming::from_interval_secs(200).lookback_for(default),
            Duration::from_secs(400),
            "lookback must be at least twice the poll interval"
        );
        assert_eq!(
            PollTiming::from_interval_secs(0).lookback_for(default),
            default
        );
    }

    #[test]
    fn test_lookback_saturates_instead_of_overflowing() {
        let timing = PollTiming::from_interval_secs(u64::MAX);
        assert!(timing.lookback_for(Duration::ZERO) >= timing.interval());
    }

    #[test]
    fn test_poll_interval_is_the_cli_interval() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel", "--interval-secs", "7"])
            .map_err(ctx("valid cli"))?;
        assert_eq!(
            PollTiming::from_interval_secs(cli.interval_secs).interval(),
            Duration::from_secs(7)
        );
        Ok(())
    }

    #[test]
    fn test_missing_system_config_falls_back_to_builtin_defaults() -> TestResult {
        let settings = settings_from_loaded(Err(not_found()), ConfigOrigin::System)
            .map_err(ctx("missing system config degrades"))?;
        assert_eq!(settings.origin, ConfigOrigin::BuiltinDefaults);
        assert!(settings.profile.is_none());
        assert_eq!(settings.sentinel_config(), SentinelConfig::default());
        Ok(())
    }

    #[test]
    fn test_missing_explicit_config_fails_closed() {
        let result = settings_from_loaded(
            Err(not_found()),
            ConfigOrigin::Explicit(std::path::PathBuf::from("/etc/harw-dod/other.toml")),
        );
        assert!(matches!(result, Err(ConfigError::Io(_))));
    }

    #[test]
    fn test_untrusted_system_config_fails_closed() {
        let result = settings_from_loaded(
            Err(ConfigError::UntrustedSystemPath {
                path: SYSTEM_CONFIG_PATH,
            }),
            ConfigOrigin::System,
        );
        assert!(matches!(
            result,
            Err(ConfigError::UntrustedSystemPath { .. })
        ));
    }

    #[test]
    fn test_other_io_errors_on_the_system_config_fail_closed() {
        let result = settings_from_loaded(
            Err(ConfigError::Io(std::io::Error::from(
                std::io::ErrorKind::PermissionDenied,
            ))),
            ConfigOrigin::System,
        );
        assert!(matches!(result, Err(ConfigError::Io(_))));
    }

    #[test]
    fn test_valid_config_resolves_the_active_profile() -> TestResult {
        let config = Config::from_toml(HOST_PROFILE).map_err(ctx("valid config"))?;
        let settings = settings_from_loaded(Ok(config), ConfigOrigin::System)
            .map_err(ctx("resolvable config"))?;
        let profile = settings
            .profile
            .as_ref()
            .ok_or(TestError::Missing("active profile"))?;
        assert_eq!(profile.profile_id().as_str(), "host");
        assert_eq!(settings.sentinel_config(), SentinelConfig::default());
        Ok(())
    }

    #[test]
    fn test_config_without_active_profile_fails_closed() -> TestResult {
        let config = Config::from_toml(&HOST_PROFILE.replace("active_profile = \"host\"\n", ""))
            .map_err(ctx("valid config without selection"))?;
        let result = settings_from_loaded(Ok(config), ConfigOrigin::System);
        assert!(matches!(result, Err(ConfigError::MissingActiveProfile)));
        Ok(())
    }

    #[test]
    fn test_health_exporter_is_absent_without_export_flags() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("valid cli"))?;
        assert!(build_health_exporter(&cli, &DodSettings::builtin_defaults()).is_none());
        Ok(())
    }

    /// [`check_export_destinations`] for `args` (after the program name)
    /// with `home` as the resolved root space.
    fn startup_check(
        home: &Path,
        args: &[&str],
    ) -> TestResult<Result<(), export::ExportPathError>> {
        let mut argv = vec!["harw-sentinel"];
        argv.extend_from_slice(args);
        let cli = Cli::try_parse_from(argv).map_err(ctx("valid cli"))?;
        Ok(check_export_destinations(&cli, home))
    }

    /// `path` as UTF-8 for a command line.
    fn utf8(path: &Path) -> TestResult<&str> {
        path.to_str().ok_or(TestError::Missing("utf8 path"))
    }

    #[test]
    fn test_startup_check_refuses_the_findings_export_path() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let result = startup_check(
            home.path(),
            &[
                "--findings-export",
                "/var/lib/harw-dod/export/findings.jsonl",
                "--health-export",
                "/var/lib/harw-dod/export/findings.jsonl",
            ],
        )?;
        assert!(matches!(
            result,
            Err(export::ExportPathError::Collision { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_startup_check_refuses_a_dot_component_alias() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let base = utf8(dir.path())?;
        let findings = format!("{base}/findings.jsonl");
        let alias = format!("{base}/./findings.jsonl");
        let result = startup_check(
            dir.path(),
            &[
                "--findings-export",
                findings.as_str(),
                "--health-export",
                alias.as_str(),
            ],
        )?;
        assert!(result.is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_startup_check_refuses_a_symlinked_parent_alias() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let real = dir.path().join("real");
        std::fs::create_dir(&real).map_err(ctx("real dir"))?;
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).map_err(ctx("symlink"))?;
        let findings = real.join("findings.jsonl");
        let alias = link.join("findings.jsonl");
        let result = startup_check(
            dir.path(),
            &[
                "--findings-export",
                utf8(&findings)?,
                "--health-export",
                utf8(&alias)?,
            ],
        )?;
        assert!(matches!(
            result,
            Err(export::ExportPathError::Collision { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_startup_check_refuses_findings_on_the_health_temporary_sibling() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let result = startup_check(
            home.path(),
            &[
                "--findings-export",
                "/var/lib/harw-dod/export/.health.json.tmp",
                "--health-export",
                "/var/lib/harw-dod/export/health.json",
            ],
        )?;
        assert!(matches!(
            result,
            Err(export::ExportPathError::SiblingOverlap { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_startup_check_accepts_the_default_health_sibling() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let findings = dir.path().join("findings.jsonl");
        startup_check(dir.path(), &["--findings-export", utf8(&findings)?])?
            .map_err(ctx("default sibling and throttle state are distinct"))?;
        Ok(())
    }

    /// PL-100 owner review: the shipped layout (exports in `<home>/export`,
    /// throttle state in `<home>`) passes.
    #[test]
    fn test_startup_check_accepts_the_shipped_layout_with_the_throttle_state() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let export_dir = home.path().join("export");
        std::fs::create_dir(&export_dir).map_err(ctx("export dir"))?;
        startup_check(
            home.path(),
            &[
                "--findings-export",
                utf8(&export_dir.join("findings.jsonl"))?,
                "--health-export",
                utf8(&export_dir.join("health.json"))?,
            ],
        )?
        .map_err(ctx("shipped layout"))?;
        Ok(())
    }

    /// PL-100 owner review: an export on the throttle state target or on its
    /// temporary sibling `.finding-throttle.json.tmp` refuses the start —
    /// for the findings export (whose writer would append to an unlinked
    /// inode after the throttle's atomic rename) and for the health
    /// document (whose rename would overwrite the throttle state every
    /// cycle).
    #[test]
    fn test_startup_check_refuses_exports_on_the_throttle_state_and_its_temporary() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = home.path().join(throttle::DEFAULT_STATE_FILE_NAME);
        let temporary = home.path().join(".finding-throttle.json.tmp");
        let health_elsewhere = home.path().join("health-elsewhere.json");
        let findings_elsewhere = home.path().join("findings-elsewhere.jsonl");

        let findings_target = startup_check(
            home.path(),
            &[
                "--findings-export",
                utf8(&target)?,
                "--health-export",
                utf8(&health_elsewhere)?,
            ],
        )?;
        assert!(
            matches!(
                findings_target,
                Err(export::ExportPathError::Collision {
                    first_role: "findings",
                    second_role: "throttle state",
                    ..
                })
            ),
            "{findings_target:?}"
        );
        let findings_temporary = startup_check(
            home.path(),
            &[
                "--findings-export",
                utf8(&temporary)?,
                "--health-export",
                utf8(&health_elsewhere)?,
            ],
        )?;
        assert!(
            matches!(
                findings_temporary,
                Err(export::ExportPathError::SiblingOverlap {
                    first_role: "findings",
                    second_role: "throttle state",
                    ..
                })
            ),
            "{findings_temporary:?}"
        );
        let health_target = startup_check(
            home.path(),
            &[
                "--findings-export",
                utf8(&findings_elsewhere)?,
                "--health-export",
                utf8(&target)?,
            ],
        )?;
        assert!(
            matches!(
                health_target,
                Err(export::ExportPathError::Collision {
                    first_role: "health",
                    second_role: "throttle state",
                    ..
                })
            ),
            "{health_target:?}"
        );
        // Ohne Befundexport: das Gesundheitsdokument allein auf der
        // temporären Geschwisterdatei des Throttle-Zustands.
        let health_temporary = startup_check(home.path(), &["--health-export", utf8(&temporary)?])?;
        assert!(
            matches!(
                health_temporary,
                Err(export::ExportPathError::SiblingOverlap {
                    first_role: "health",
                    second_role: "throttle state",
                    ..
                })
            ),
            "{health_temporary:?}"
        );
        Ok(())
    }

    /// A symlinked alias of `--home` cannot smuggle an export onto the
    /// throttle state either.
    #[cfg(unix)]
    #[test]
    fn test_startup_check_refuses_a_symlinked_alias_of_the_throttle_state() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = dir.path().join("home");
        std::fs::create_dir(&home).map_err(ctx("home dir"))?;
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&home, &link).map_err(ctx("symlink"))?;
        let alias = link.join(throttle::DEFAULT_STATE_FILE_NAME);
        let result = startup_check(&home, &["--findings-export", utf8(&alias)?])?;
        assert!(
            matches!(
                result,
                Err(export::ExportPathError::Collision {
                    second_role: "throttle state",
                    ..
                })
            ),
            "{result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_startup_check_refuses_an_unresolvable_home() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let result = startup_check(&dir.path().join("missing"), &[])?;
        assert!(matches!(
            result,
            Err(export::ExportPathError::Unresolved { .. })
        ));
        Ok(())
    }

    /// PL-100 owner review, start level: `run` refuses an export on the
    /// throttle state before any writer, socket or Landlock step — nothing
    /// is written to the state target and no socket is bound.
    #[test]
    fn test_run_refuses_an_export_on_the_throttle_state_before_any_writer() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = home.path().join(throttle::DEFAULT_STATE_FILE_NAME);
        let socket = home.path().join("s.sock");
        let cli = Cli::try_parse_from([
            "harw-sentinel",
            "--once",
            "--home",
            utf8(home.path())?,
            "--socket",
            utf8(&socket)?,
            "--findings-export",
            utf8(&target)?,
            "--findings-host",
            "host-a",
        ])
        .map_err(ctx("valid cli"))?;

        let result = run(cli, DodSettings::builtin_defaults());

        assert!(
            matches!(
                result,
                Err(SentinelBinError::Destinations(
                    export::ExportPathError::Collision { .. }
                ))
            ),
            "{result:?}"
        );
        assert!(!target.exists(), "no writer touched the state target");
        assert!(!socket.exists(), "no socket was bound");
        Ok(())
    }

    #[test]
    fn test_health_exporter_defaults_next_to_the_findings_export() -> TestResult {
        let cli = Cli::try_parse_from([
            "harw-sentinel",
            "--findings-export",
            "/var/lib/harw-dod/export/findings.jsonl",
        ])
        .map_err(ctx("valid cli"))?;
        let health = build_health_exporter(&cli, &DodSettings::builtin_defaults())
            .ok_or(TestError::Missing("default health exporter"))?;
        assert_eq!(
            health.path(),
            Path::new("/var/lib/harw-dod/export/health.json")
        );
        Ok(())
    }

    #[test]
    fn test_explicit_relative_config_path_is_rejected() {
        let result = load_dod_settings(Some(Path::new("config.toml")));
        assert!(matches!(
            result,
            Err(ConfigError::InvalidExplicitPath { .. })
        ));
    }

    /// Der Bericht von `--print-sandbox` als Zeilen (PL-100 WP0.7), ohne
    /// nutzbare DoD-Konfiguration.
    fn sandbox_report(args: &[&str]) -> TestResult<Vec<String>> {
        sandbox_report_with(args, None)
    }

    /// Der Bericht von `--print-sandbox` mit `settings`.
    fn sandbox_report_with(
        args: &[&str],
        settings: Option<&DodSettings>,
    ) -> TestResult<Vec<String>> {
        let mut argv = vec!["harw-sentinel", "--print-sandbox"];
        argv.extend_from_slice(args);
        let cli = Cli::try_parse_from(argv).map_err(ctx("valid cli"))?;
        let mut out: Vec<u8> = Vec::new();
        run_print_sandbox(&cli, settings, &mut out).map_err(ctx("print sandbox"))?;
        let text = String::from_utf8(out).map_err(ctx("utf8 report"))?;
        Ok(text.lines().map(str::to_owned).collect())
    }

    /// PL-100 WP0.7, Abnahme „Ruleset ≠ /": selbst ein ausdrückliches
    /// `--workspace-root /` wird keine Leseregel, sondern abgewiesen.
    #[test]
    fn test_print_sandbox_refuses_the_filesystem_root_as_workspace() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home_arg = home
            .path()
            .to_str()
            .ok_or(TestError::Missing("utf8 home"))?;
        let lines = sandbox_report(&["--home", home_arg, "--workspace-root", "/"])?;

        assert!(
            lines
                .iter()
                .any(|line| line == "landlock-refused\tfilesystem-root\t\"/\""),
            "the root must be reported as refused: {lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.starts_with("landlock\t") && line.ends_with("\t\"/\"")),
            "no rule may name the filesystem root: {lines:?}"
        );
        assert!(
            lines.contains(&format!("landlock\tread-write\t{:?}", home.path())),
            "home is the writable root: {lines:?}"
        );
        Ok(())
    }

    #[test]
    fn test_print_sandbox_without_workspace_root_lists_no_working_directory() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home_arg = home
            .path()
            .to_str()
            .ok_or(TestError::Missing("utf8 home"))?;
        let cwd = std::env::current_dir().map_err(ctx("cwd"))?;
        let lines = sandbox_report(&["--home", home_arg])?;

        assert!(
            !lines
                .iter()
                .any(|line| line.ends_with(&format!("\t{cwd:?}"))),
            "the working directory is never a root: {lines:?}"
        );
        assert!(
            lines.iter().any(|line| line == "ipc-peer-uid\t0\talways"),
            "root is always an admitted peer: {lines:?}"
        );
        Ok(())
    }

    #[test]
    fn test_print_sandbox_reports_numeric_probe_peers() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home_arg = home
            .path()
            .to_str()
            .ok_or(TestError::Missing("utf8 home"))?;
        let lines = sandbox_report(&["--home", home_arg, "--probe-peer-user", "4242"])?;
        assert!(
            lines
                .iter()
                .any(|line| line == "ipc-peer-uid\t4242\t\"4242\""),
            "{lines:?}"
        );
        Ok(())
    }

    #[test]
    fn test_landlock_roots_add_each_export_directory_once() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("valid cli"))?;
        let export_dir = Path::new("/var/lib/harw-dod/export");
        let same = landlock_roots(
            &cli,
            Path::new("/var/lib/harw-dod"),
            Some(export_dir),
            Some(export_dir),
        );
        let other = landlock_roots(
            &cli,
            Path::new("/var/lib/harw-dod"),
            Some(export_dir),
            Some(Path::new("/run/harw-dod")),
        );
        let writable = |roots: &[sandbox::SandboxRoot]| roots.iter().filter(|r| r.writable).count();
        assert_eq!(writable(same.as_slice()), 2, "home + one export directory");
        assert_eq!(
            writable(other.as_slice()),
            3,
            "home + two export directories"
        );
        assert!(
            same.iter().all(|root| root.path.is_absolute()),
            "no default root depends on the working directory"
        );
        Ok(())
    }

    // --- PL-100 Schritt 2: `[sentinel]`, Listener-Historie, Landlock ------

    /// `HOST_PROFILE` mit einer Tabelle `[sentinel]`, aufgelöst wie in
    /// [`main`].
    fn settings_with_sentinel(table: &str) -> TestResult<DodSettings> {
        let config = Config::from_toml(&format!("{HOST_PROFILE}\n[sentinel]\n{table}\n"))
            .map_err(ctx("valid config with [sentinel]"))?;
        settings_from_loaded(Ok(config), ConfigOrigin::System).map_err(ctx("resolvable config"))
    }

    /// PL-100: das Flag gewinnt, sonst `[sentinel]`, sonst die Vorgabe —
    /// für Sonden-Konten und Abklingzeit.
    #[test]
    fn test_effective_sentinel_values_prefer_flag_then_config_then_default() -> TestResult {
        let settings = settings_with_sentinel(
            "probe_peer_users = [\"svc-probe\"]\nfinding_cooldown_secs = 120",
        )?;
        let bare = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("valid cli"))?;
        assert_eq!(
            effective_probe_peer_users(&bare, Some(&settings)),
            (vec!["svc-probe".to_owned()], SettingSource::DodConfig)
        );
        assert_eq!(
            effective_finding_cooldown_secs(&bare, &settings),
            (120, SettingSource::DodConfig)
        );

        let flags = Cli::try_parse_from([
            "harw-sentinel",
            "--probe-peer-user",
            "991",
            "--finding-cooldown-secs",
            "0",
        ])
        .map_err(ctx("valid cli"))?;
        assert_eq!(
            effective_probe_peer_users(&flags, Some(&settings)),
            (vec!["991".to_owned()], SettingSource::CommandLine)
        );
        assert_eq!(
            effective_finding_cooldown_secs(&flags, &settings),
            (0, SettingSource::CommandLine)
        );

        let defaults = DodSettings::builtin_defaults();
        let built_in = (
            vec!["harw-probe-bpf".to_owned(), "harw-probe-fs".to_owned()],
            SettingSource::BuiltIn,
        );
        assert_eq!(effective_probe_peer_users(&bare, Some(&defaults)), built_in);
        assert_eq!(effective_probe_peer_users(&bare, None), built_in);
        assert_eq!(
            effective_finding_cooldown_secs(&bare, &defaults),
            (throttle::DEFAULT_COOLDOWN_SECS, SettingSource::BuiltIn)
        );

        // Ein einzelner Schlüssel lässt den anderen bei der Vorgabe; eine
        // leere Liste ist eine Angabe (nur uid 0), keine fehlende.
        let partial = settings_with_sentinel("probe_peer_users = []")?;
        assert_eq!(
            effective_probe_peer_users(&bare, Some(&partial)),
            (Vec::new(), SettingSource::DodConfig)
        );
        assert_eq!(
            effective_finding_cooldown_secs(&bare, &partial).1,
            SettingSource::BuiltIn
        );
        Ok(())
    }

    /// PL-100 WP1.8: `--container-sensor` gewinnt, sonst `[sentinel]
    /// container_sensor`, sonst `auto`.
    #[test]
    fn test_effective_container_sensor_prefers_flag_then_config_then_auto() -> TestResult {
        let settings = settings_with_sentinel("container_sensor = \"off\"")?;
        let bare = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("valid cli"))?;
        assert_eq!(
            effective_container_sensor(&bare, Some(&settings)),
            (ContainerSensorMode::Off, SettingSource::DodConfig)
        );
        let flag = Cli::try_parse_from(["harw-sentinel", "--container-sensor", "on"])
            .map_err(ctx("valid cli"))?;
        assert_eq!(
            effective_container_sensor(&flag, Some(&settings)),
            (ContainerSensorMode::On, SettingSource::CommandLine)
        );
        let defaults = DodSettings::builtin_defaults();
        for settings in [Some(&defaults), None] {
            assert_eq!(
                effective_container_sensor(&bare, settings),
                (ContainerSensorMode::Auto, SettingSource::BuiltIn)
            );
        }
        Ok(())
    }

    /// PL-100 WP1.8: der Plan folgt dem wirksamen Schalter; mit `off` aus
    /// der Konfiguration gibt es keinen Sensor, mit `--container-sensor on`
    /// jede Anordnung — auch auf einem Host ohne Laufzeit.
    #[test]
    fn test_container_plan_follows_the_effective_switch() -> TestResult {
        let cgroup = tempfile::tempdir().map_err(ctx("cgroup root"))?;
        let run = tempfile::tempdir().map_err(ctx("run root"))?;
        let cgroup_arg = cgroup
            .path()
            .to_str()
            .ok_or(TestError::Missing("utf8 cgroup root"))?;
        let settings = settings_with_sentinel("container_sensor = \"off\"")?;

        let from_config = Cli::try_parse_from(["harw-sentinel", "--cgroup-root", cgroup_arg])
            .map_err(ctx("valid cli"))?;
        let (plan, source) = plan_container_sensors(&from_config, Some(&settings), run.path());
        assert!(plan.layouts.is_empty());
        assert_eq!(source, SettingSource::DodConfig);

        let forced = Cli::try_parse_from([
            "harw-sentinel",
            "--cgroup-root",
            cgroup_arg,
            "--container-sensor",
            "on",
        ])
        .map_err(ctx("valid cli"))?;
        let (plan, source) = plan_container_sensors(&forced, Some(&settings), run.path());
        assert_eq!(
            plan.layout_kinds(),
            container_plan::ContainerLayout::ALL.to_vec()
        );
        assert_eq!(source, SettingSource::CommandLine);
        Ok(())
    }

    /// Ein Fixture-Host für die Container-Komposition: cgroup-v2-Wurzel
    /// (`cgroup.controllers`), `/run`, procfs und Root-Space.
    struct ContainerHost {
        cgroup: tempfile::TempDir,
        run: tempfile::TempDir,
        proc_root: tempfile::TempDir,
        home: tempfile::TempDir,
    }

    impl ContainerHost {
        fn new() -> TestResult<Self> {
            let host = Self {
                cgroup: tempfile::tempdir().map_err(ctx("cgroup root"))?,
                run: tempfile::tempdir().map_err(ctx("run root"))?,
                proc_root: tempfile::tempdir().map_err(ctx("proc root"))?,
                home: tempfile::tempdir().map_err(ctx("home"))?,
            };
            std::fs::write(
                host.cgroup.path().join("cgroup.controllers"),
                b"cpu memory\n",
            )
            .map_err(ctx("cgroup.controllers"))?;
            Ok(host)
        }

        fn socket(&self, relative: &str) -> TestResult {
            let path = self.run.path().join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(ctx("socket parent"))?;
            }
            std::fs::write(path, b"").map_err(ctx("socket stand-in"))
        }

        /// Plan über den Produktionsweg (`plan_container_sensors`, Schalter
        /// `auto`) und daraus nur die Container-Sensoren von
        /// `sensors::build_sensors`.
        fn plan_and_sensors(
            &self,
        ) -> TestResult<(
            container_plan::ContainerPlan,
            Vec<Arc<dyn harw_dod_signals::Sensor>>,
        )> {
            let cgroup_arg = self
                .cgroup
                .path()
                .to_str()
                .ok_or(TestError::Missing("utf8 cgroup root"))?;
            let cli = Cli::try_parse_from(["harw-sentinel", "--cgroup-root", cgroup_arg])
                .map_err(ctx("valid cli"))?;
            let (plan, _) = plan_container_sensors(&cli, None, self.run.path());
            let roots = SensorRoots {
                proc_root: self.proc_root.path().to_path_buf(),
                thermal_root: self.proc_root.path().to_path_buf(),
                workspace_root: None,
                blockio_root: self.proc_root.path().to_path_buf(),
                gpu_root: self.proc_root.path().to_path_buf(),
                cgroup_root: self.cgroup.path().to_path_buf(),
                home: self.home.path().to_path_buf(),
                containers: plan.layouts.clone(),
            };
            let containers: Vec<Arc<dyn harw_dod_signals::Sensor>> = sensors::build_sensors(&roots)
                .into_iter()
                .filter(|sensor| {
                    sensor.handle().capability() == harw_dod_cap::Capability::ReadContainerScopes
                })
                .collect();
            Ok((plan, containers))
        }
    }

    /// Ob der Ring ein `SensorDegraded`-Ereignis enthält.
    fn has_sensor_degraded(sentinel: &Sentinel) -> bool {
        sentinel.buffer().events().any(|event| {
            matches!(
                event.kind,
                harw_dod_signals::EventKind::SensorDegraded { .. }
            )
        })
    }

    /// Review Runde 1 (PL-100 WP1.8): Podman-Socket vorhanden, aber noch kein
    /// `machine.slice` (kein Container seit dem Boot). Der über den
    /// Produktionsweg geplante und gebaute Container-Sensor wird in zwei
    /// `poll_once`-Zyklen nie stillgelegt — kein `SensorDegraded`, kein
    /// Befund, `health.json` meldet ihn `bound` — und sieht den ersten
    /// danach gestarteten Container.
    #[test]
    fn test_a_podman_socket_without_machine_slice_never_retires_the_container_sensor() -> TestResult
    {
        let host = ContainerHost::new()?;
        host.socket("podman/podman.sock")?;
        let (plan, sensors) = host.plan_and_sensors()?;
        assert_eq!(
            plan.layout_kinds(),
            container_plan::ContainerLayout::ALL.to_vec()
        );
        assert_eq!(sensors.len(), container_plan::ContainerLayout::ALL.len());
        assert!(!host.cgroup.path().join("machine.slice").exists());

        let mut sentinel = Sentinel::new(
            sensors,
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        let health_path = host.home.path().join("health.json");
        let mut health = health_export::HealthExporter::new(
            health_path.clone(),
            health_export::ConfigIdentity::default(),
        );
        let findings_host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter =
            export::FindingsExporter::new(host.home.path().join("findings.jsonl"), findings_host);
        let mut rules = rule_cycle();
        for _ in 0..2 {
            poll_once(
                &mut sentinel,
                None,
                &harw_observe::NullSink,
                &mut rules,
                Some(&mut exporter),
                Some(&mut health),
                None,
            );
        }

        assert!(!has_sensor_degraded(&sentinel));
        assert_eq!(exporter.written(), 0, "no sensor-degraded finding");
        assert_eq!(sentinel.degraded_count(), 0);
        assert!(
            sentinel
                .health_snapshot()
                .iter()
                .all(|(_, health)| matches!(health, harw_dod_sentinel::SensorHealth::Bound))
        );
        let document: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&health_path).map_err(ctx("read health"))?,
        )
        .map_err(ctx("parse health"))?;
        let entry = document
            .get("sensors")
            .and_then(serde_json::Value::as_array)
            .and_then(|sensors| {
                sensors.iter().find(|sensor| {
                    sensor.get("id") == Some(&serde_json::json!("container-machine"))
                })
            })
            .ok_or(TestError::Missing("container-machine in health.json"))?;
        assert_eq!(entry.get("health"), Some(&serde_json::json!("bound")));
        assert_eq!(entry.get("errors_total"), Some(&serde_json::json!(0)));

        // Der erste Container legt `machine.slice` an — der Sensor sieht ihn.
        std::fs::create_dir_all(
            host.cgroup
                .path()
                .join("machine.slice")
                .join(format!("libpod-{}.scope", "a".repeat(64))),
        )
        .map_err(ctx("first container scope"))?;
        poll_once(
            &mut sentinel,
            None,
            &harw_observe::NullSink,
            &mut rules,
            Some(&mut exporter),
            Some(&mut health),
            None,
        );
        assert!(
            sentinel
                .buffer()
                .samples()
                .any(|sample| sample.metric == "container_unowned_aaaaaaaaaaaa")
        );
        assert!(!has_sensor_degraded(&sentinel));
        Ok(())
    }

    /// Review R2 (PL-100 WP1.8, B19): `auto` ohne Laufzeit-Socket und ohne
    /// `machine.slice` (daemonloses rootful Podman, Sentinel startet beim
    /// Boot vor jedem Container) registriert trotzdem jeden
    /// Container-Sensor; die Zyklen davor bleiben still (kein
    /// `SensorDegraded`, kein Befund), und der erste später gestartete
    /// `libpod-*.scope` wird über `poll_once` gesehen.
    #[test]
    fn test_auto_without_any_runtime_observes_a_later_podman_container() -> TestResult {
        let host = ContainerHost::new()?;
        let (plan, sensors) = host.plan_and_sensors()?;
        assert_eq!(
            plan.layouts
                .iter()
                .map(|planned| planned.availability_label())
                .collect::<Vec<_>>(),
            vec!["on-demand"; container_plan::ContainerLayout::ALL.len()]
        );
        assert_eq!(sensors.len(), container_plan::ContainerLayout::ALL.len());

        let mut sentinel = Sentinel::new(
            sensors,
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        let findings_host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter =
            export::FindingsExporter::new(host.home.path().join("findings.jsonl"), findings_host);
        let mut rules = rule_cycle();
        for _ in 0..2 {
            poll_once(
                &mut sentinel,
                None,
                &harw_observe::NullSink,
                &mut rules,
                Some(&mut exporter),
                None,
                None,
            );
        }
        assert!(!has_sensor_degraded(&sentinel));
        assert_eq!(sentinel.degraded_count(), 0);
        assert_eq!(exporter.written(), 0, "a host without containers is quiet");

        std::fs::create_dir_all(
            host.cgroup
                .path()
                .join("machine.slice")
                .join(format!("libpod-{}.scope", "c".repeat(64))),
        )
        .map_err(ctx("later podman container scope"))?;
        poll_once(
            &mut sentinel,
            None,
            &harw_observe::NullSink,
            &mut rules,
            Some(&mut exporter),
            None,
            None,
        );
        assert!(
            sentinel
                .buffer()
                .samples()
                .any(|sample| sample.metric == "container_unowned_cccccccccccc")
        );
        assert!(!has_sensor_degraded(&sentinel));
        Ok(())
    }

    /// Review R2 („degraded otherwise"): ohne cgroup-v2-Wurzel kann kein
    /// Container-Sensor Scopes sehen — `auto` plant jede Anordnung als
    /// `required`, und der erste Zyklus meldet jeden Container-Sensor als
    /// beeinträchtigt (`SensorDegraded`, `sensor-degraded`-Befund) statt still
    /// blind zu sein.
    #[test]
    fn test_auto_without_a_cgroup_v2_root_reports_the_container_sensors_degraded() -> TestResult {
        let host = ContainerHost::new()?;
        std::fs::remove_file(host.cgroup.path().join("cgroup.controllers"))
            .map_err(ctx("drop the cgroup v2 marker"))?;
        let (plan, sensors) = host.plan_and_sensors()?;
        assert!(
            plan.layouts
                .iter()
                .all(|planned| planned.availability_label() == "required")
        );

        let mut sentinel = Sentinel::new(
            sensors,
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        let export_dir = host.home.path().join("export");
        std::fs::create_dir(&export_dir).map_err(ctx("export dir"))?;
        let findings = export_dir.join("findings.jsonl");
        let findings_host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = export::FindingsExporter::new(findings.clone(), findings_host);
        poll_once(
            &mut sentinel,
            None,
            &harw_observe::NullSink,
            &mut rule_cycle(),
            Some(&mut exporter),
            None,
            None,
        );

        assert!(has_sensor_degraded(&sentinel));
        assert_eq!(
            sentinel.degraded_count(),
            container_plan::ContainerLayout::ALL.len()
        );
        let lines = client_lines(&findings)?;
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|line| line.rule_id == "sensor-degraded"));
        for layout in container_plan::ContainerLayout::ALL {
            assert!(
                lines
                    .iter()
                    .any(|line| line.summary.contains(layout.sensor_id())),
                "{} reported degraded",
                layout.sensor_id()
            );
        }
        Ok(())
    }

    /// Review Runde 1 (PL-100 WP1.8, B19): Docker mit `cgroupfs`-Treiber ist
    /// keine stille Blindheit — der Start reicht ein `SensorDegraded` nach,
    /// und der erste Zyklus exportiert genau einen `sensor-degraded`-Befund
    /// mit der Meldekennung der Anordnung.
    #[test]
    fn test_the_docker_cgroupfs_layout_is_reported_as_sensor_degraded() -> TestResult {
        let host = ContainerHost::new()?;
        host.socket("docker.sock")?;
        std::fs::create_dir_all(host.cgroup.path().join("docker").join("b".repeat(64)))
            .map_err(ctx("cgroupfs container"))?;
        std::fs::create_dir_all(host.cgroup.path().join("system.slice").join("sshd.service"))
            .map_err(ctx("system.slice"))?;
        let (plan, sensors) = host.plan_and_sensors()?;
        assert_eq!(
            plan.unsupported,
            vec![container_plan::UnsupportedLayout::DockerCgroupfs]
        );

        let mut sentinel = Sentinel::new(
            sensors,
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        assert_eq!(
            record_unsupported_container_layouts(
                &plan.unsupported,
                &mut sentinel,
                Timestamp::UNIX_EPOCH
            ),
            1
        );
        let export_dir = host.home.path().join("export");
        std::fs::create_dir(&export_dir).map_err(ctx("export dir"))?;
        let findings = export_dir.join("findings.jsonl");
        let findings_host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
        let mut exporter = export::FindingsExporter::new(findings.clone(), findings_host);
        poll_once(
            &mut sentinel,
            None,
            &harw_observe::NullSink,
            &mut rule_cycle(),
            Some(&mut exporter),
            None,
            None,
        );

        assert_eq!(exporter.written(), 1);
        let lines = client_lines(&findings)?;
        let line = lines.first().ok_or(TestError::Missing("export line"))?;
        assert_eq!(line.rule_id, "sensor-degraded");
        assert!(
            line.summary
                .contains(container_plan::UnsupportedLayout::DockerCgroupfs.status_sensor_id()),
            "{}",
            line.summary
        );
        // Der Docker-Sensor selbst bleibt gebunden (sein Verzeichnis existiert).
        assert_eq!(sentinel.degraded_count(), 0);
        Ok(())
    }

    /// Review Runde 1: `--print-sandbox` nennt die Verfügbarkeitsregel jeder
    /// Anordnung und jede nicht unterstützte Anordnung.
    #[test]
    fn test_container_report_lists_availability_and_unsupported_layouts() -> TestResult {
        let plan = container_plan::ContainerPlan {
            mode: ContainerSensorMode::Auto,
            layouts: vec![container_plan::PlannedLayout {
                layout: container_plan::ContainerLayout::Machine,
                missing_slice: harw_dod_container::MissingRoot::Empty,
            }],
            unsupported: vec![container_plan::UnsupportedLayout::DockerCgroupfs],
        };
        let mut out = Vec::new();
        write_container_report(&mut out, &plan, SettingSource::BuiltIn)
            .map_err(ctx("write report"))?;
        let text = String::from_utf8(out).map_err(ctx("utf8 report"))?;
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines,
            vec![
                "container-sensor\tauto\tbuilt-in",
                "container-layout\tpodman-machine-slice\tcontainer-machine\ton-demand",
                "container-unsupported\tdocker-cgroupfs\tcontainer-docker-cgroupfs",
            ]
        );
        Ok(())
    }

    /// PL-100 WP1.10: mit den sysfs-Vorgaben ist `/sys/devices` genau
    /// einmal eine nur lesbare Landlock-Wurzel; mit Fixture-Wurzeln außerhalb
    /// von `/sys` nicht.
    #[test]
    fn test_landlock_roots_add_the_sysfs_alias_prefix_read_only() -> TestResult {
        let cli = Cli::try_parse_from(["harw-sentinel"]).map_err(ctx("valid cli"))?;
        let roots = landlock_roots(&cli, Path::new("/var/lib/harw-dod"), None, None);
        let devices: Vec<&sandbox::SandboxRoot> = roots
            .iter()
            .filter(|root| root.path == Path::new("/sys/devices"))
            .collect();
        assert_eq!(devices.len(), 1, "{roots:?}");
        assert!(devices.iter().all(|root| !root.writable));
        // `/proc` bleibt eine nur lesbare Wurzel (Sensoren unter `/proc`,
        // Container-Sensor `/proc/<pid>/status`).
        assert!(
            roots
                .iter()
                .any(|root| root.path == Path::new("/proc") && !root.writable),
            "{roots:?}"
        );

        let fixture = Cli::try_parse_from([
            "harw-sentinel",
            "--thermal-root",
            "/srv/fixture/thermal",
            "--blockio-root",
            "/srv/fixture/block",
            "--gpu-root",
            "/srv/fixture/drm",
        ])
        .map_err(ctx("valid cli"))?;
        let roots = landlock_roots(&fixture, Path::new("/var/lib/harw-dod"), None, None);
        assert!(
            roots
                .iter()
                .all(|root| root.path != Path::new("/sys/devices")),
            "{roots:?}"
        );
        Ok(())
    }

    /// PL-100 WP1.8: `--print-sandbox` nennt Schalter und Herkunft des
    /// Container-Sensors.
    #[test]
    fn test_print_sandbox_reports_the_container_switch() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home_arg = home
            .path()
            .to_str()
            .ok_or(TestError::Missing("utf8 home"))?;
        let lines = sandbox_report(&["--home", home_arg, "--container-sensor", "off"])?;
        assert!(
            lines
                .iter()
                .any(|line| line == "container-sensor\toff\tcommand-line"),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.starts_with("container-layout\t")),
            "{lines:?}"
        );
        Ok(())
    }

    /// Eine Lauscher-Meldung des Listener-Sensors ohne Akteur.
    fn listener_opened(port: u16) -> harw_dod_signals::SecurityEvent {
        harw_dod_signals::SecurityEvent {
            sensor: harw_types::SensorId::from_str("listener-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: harw_dod_signals::EventKind::ListenerOpened { port },
        }
    }

    /// Eine Kommandozeile mit `--home` und Befundexport unter `<home>/export`.
    fn export_cli(home: &Path, findings: &Path) -> TestResult<Cli> {
        Cli::try_parse_from([
            "harw-sentinel",
            "--home",
            utf8(home)?,
            "--findings-export",
            utf8(findings)?,
            "--findings-host",
            "host-a",
        ])
        .map_err(ctx("valid cli"))
    }

    /// Die unter `home` gespeicherte Listener-Historie, gelesen wie beim
    /// Start (`ListenerHistory::from_json`).
    fn stored_listener_history(home: &Path) -> TestResult<harw_dod_rules::ListenerHistory> {
        let bytes =
            std::fs::read(listener_state_path(home)).map_err(ctx("read listener history"))?;
        harw_dod_rules::ListenerHistory::from_json(&bytes).map_err(ctx("parse listener history"))
    }

    /// PL-100 WP1.6, Kompositionsebene: der Produktionsbauer lädt und
    /// speichert die Listener-Historie unter `--home`. Der erste Start lernt
    /// still, ein späterer meldet nur den seitdem neuen Listener, und auch
    /// der ist nach dem nächsten Neustart bekannt (owner thread
    /// 4505153640). Damit der persistierte Finding-Throttle das nicht
    /// verdeckt, prüft der Test die gespeicherte Historie direkt und löscht
    /// vor dem dritten Start den Throttle-Zustand: ein neu gelernter 4444
    /// erschiene sonst als zweite Exportzeile.
    #[test]
    fn test_restart_keeps_the_listener_history_and_reports_only_new_listeners() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let export_dir = home.path().join("export");
        std::fs::create_dir(&export_dir).map_err(ctx("export dir"))?;
        let findings = export_dir.join("findings.jsonl");
        let cli = export_cli(home.path(), &findings)?;
        let settings = DodSettings::builtin_defaults();

        let first = start_with(
            &cli,
            &settings,
            home.path(),
            vec![listener_opened(22)],
            None,
        )?;
        assert_eq!(first.written(), 0, "the first start only learns");
        assert!(
            listener_state_path(home.path()).is_file(),
            "the learning cycle is persisted at once"
        );

        let second = start_with(
            &cli,
            &settings,
            home.path(),
            vec![listener_opened(22), listener_opened(4444)],
            None,
        )?;
        assert_eq!(second.written(), 1, "only the listener opened since");

        // Die gespeicherte Historie selbst: geprimt, beide Listener bekannt.
        let key_4444 = harw_dod_rules::ListenerKey::from_event(&listener_opened(4444), None)
            .ok_or(TestError::Missing("listener key of port 4444"))?;
        let stored = stored_listener_history(home.path())?;
        assert!(stored.is_primed(), "the history stays primed");
        assert_eq!(stored.len(), 2, "22 and 4444");
        assert!(
            stored.contains(&key_4444),
            "4444 is persisted after start 2"
        );

        // Ohne Throttle-Zustand unterdrückte nichts einen erneut als neu
        // gemeldeten 4444; nur die Historie hält ihn bekannt.
        let throttle = throttle_state_path(home.path());
        assert!(throttle.is_file(), "the reported finding stored its stamp");
        std::fs::remove_file(&throttle).map_err(ctx("remove throttle state"))?;

        let third = start_with(
            &cli,
            &settings,
            home.path(),
            vec![listener_opened(22), listener_opened(4444)],
            None,
        )?;
        assert_eq!(third.written(), 0, "4444 was learned before the restart");
        let reloaded = stored_listener_history(home.path())?;
        assert_eq!(
            reloaded.primed_at(),
            stored.primed_at(),
            "start 3 reloaded the history instead of learning anew"
        );
        assert_eq!(reloaded.len(), 2);
        assert!(reloaded.contains(&key_4444), "a reload keeps 4444 known");

        let lines = client_lines(&findings)?;
        assert_eq!(
            lines
                .iter()
                .map(|line| line.rule_id.as_str())
                .collect::<Vec<_>>(),
            vec!["listener-new"]
        );
        Ok(())
    }

    /// Ein JSON-Wert unter `path` (Objektschlüssel), ohne Indexoperator.
    fn json_at<'a>(value: &'a serde_json::Value, path: &[&str]) -> Option<&'a serde_json::Value> {
        path.iter().try_fold(value, |value, key| value.get(*key))
    }

    /// PL-100 WP1.6: eine unbrauchbare gespeicherte Historie bricht den
    /// Start nicht ab, wird aber nie still neu gelernt — der erste Zyklus
    /// exportiert einen `sensor-degraded`-Befund, `health.json` meldet
    /// `listener_history` als `degraded`, und die neu gelernte Historie ist
    /// danach wieder gültig gespeichert.
    #[test]
    fn test_unusable_listener_history_is_reported_as_sensor_degraded_and_in_health() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let export_dir = home.path().join("export");
        std::fs::create_dir(&export_dir).map_err(ctx("export dir"))?;
        let findings = export_dir.join("findings.jsonl");
        std::fs::write(listener_state_path(home.path()), b"not a listener history")
            .map_err(ctx("corrupt listener history"))?;
        let cli = export_cli(home.path(), &findings)?;
        let settings = DodSettings::builtin_defaults();
        let mut health =
            build_health_exporter(&cli, &settings).ok_or(TestError::Missing("health exporter"))?;

        let exporter = start_with(
            &cli,
            &settings,
            home.path(),
            vec![listener_opened(22)],
            Some(&mut health),
        )?;

        assert_eq!(exporter.written(), 1, "exactly the sensor-degraded finding");
        let lines = client_lines(&findings)?;
        let line = lines.first().ok_or(TestError::Missing("export line"))?;
        assert_eq!(line.rule_id, "sensor-degraded");
        assert!(
            line.summary
                .contains(listener_state::LISTENER_HISTORY_STATUS_SENSOR_ID),
            "{}",
            line.summary
        );

        let document: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(export_dir.join(health_export::DEFAULT_HEALTH_FILE_NAME))
                .map_err(ctx("read health"))?,
        )
        .map_err(ctx("parse health"))?;
        let field = |name: &str| json_at(&document, &["listener_history", name]).cloned();
        assert_eq!(field("state"), Some(serde_json::json!("degraded")));
        assert_eq!(
            field("degrade_reason"),
            Some(serde_json::json!("state_invalid"))
        );
        assert_eq!(field("primed"), Some(serde_json::json!(true)));
        assert_eq!(field("keys"), Some(serde_json::json!(1)));

        let stored =
            std::fs::read(listener_state_path(home.path())).map_err(ctx("read history"))?;
        let relearned = harw_dod_rules::ListenerHistory::from_json(&stored)
            .map_err(ctx("the relearned history is valid again"))?;
        assert!(relearned.is_primed());
        Ok(())
    }

    /// PL-100 WP0.7 bis zum Leser: `poll_once` schreibt den Landlock-Bericht
    /// und den Stand der Listener-Historie, und `harw-dod-client` liest den
    /// Landlock-Bericht mit Ergebnis und ABI-Stufen.
    #[test]
    fn test_health_carries_landlock_and_listener_history_through_to_the_client() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(health_export::DEFAULT_HEALTH_FILE_NAME);
        let proof = sandbox::SandboxApplied::assumed_with_abi_for_test(
            sandbox::SandboxOutcome::PartiallyEnforced,
            Some(4),
        );
        let mut health = health_export::HealthExporter::new(
            path.clone(),
            health_export::ConfigIdentity::default(),
        )
        .with_landlock(proof.report());
        let mut sentinel = Sentinel::new(
            Vec::new(),
            Arc::new(harw_observe::NullSink),
            SentinelConfig::default(),
        );
        sentinel.record_external_event(listener_opened(22));
        let mut rules = rule_cycle().with_listener_state(listener_state::ListenerState::open(None));

        poll_once(
            &mut sentinel,
            None,
            &harw_observe::NullSink,
            &mut rules,
            None,
            Some(&mut health),
            None,
        );

        let report = harw_dod_client::health::read_health(&path)
            .map_err(ctx("harw-dod-client reads the health document"))?
            .ok_or(TestError::Missing("health document"))?;
        let landlock = report
            .landlock
            .ok_or(TestError::Missing("landlock report"))?;
        assert_eq!(
            landlock.outcome,
            harw_dod_client::LandlockOutcome::PartiallyEnforced
        );
        assert_eq!(
            landlock.requested_abi,
            Some(u32::from(sandbox::REQUESTED_ABI))
        );
        assert_eq!(landlock.effective_abi, Some(4));

        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).map_err(ctx("read health"))?)
                .map_err(ctx("parse health"))?;
        assert_eq!(
            json_at(&document, &["listener_history", "state"]),
            Some(&serde_json::json!("ok"))
        );
        assert_eq!(
            json_at(&document, &["listener_history", "primed"]),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            json_at(&document, &["listener_history", "keys"]),
            Some(&serde_json::json!(1))
        );
        Ok(())
    }

    /// PL-100 WP1.6: auch die Listener-Historie und ihre temporäre
    /// Geschwisterdatei sind Schreibziele der Alias-Prüfung.
    #[test]
    fn test_startup_check_refuses_exports_on_the_listener_history_and_its_temporary() -> TestResult
    {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = home.path().join(listener_state::DEFAULT_STATE_FILE_NAME);
        let temporary = home.path().join(".listener-history.json.tmp");
        let health_elsewhere = home.path().join("health-elsewhere.json");

        let findings_target = startup_check(
            home.path(),
            &[
                "--findings-export",
                utf8(&target)?,
                "--health-export",
                utf8(&health_elsewhere)?,
            ],
        )?;
        assert!(
            matches!(
                findings_target,
                Err(export::ExportPathError::Collision {
                    first_role: "findings",
                    second_role: "listener history",
                    ..
                })
            ),
            "{findings_target:?}"
        );
        let health_temporary = startup_check(home.path(), &["--health-export", utf8(&temporary)?])?;
        assert!(
            matches!(
                health_temporary,
                Err(export::ExportPathError::SiblingOverlap {
                    first_role: "health",
                    second_role: "listener history",
                    ..
                })
            ),
            "{health_temporary:?}"
        );
        Ok(())
    }

    /// PL-100 review round 1: `--socket` on the listener history (or the
    /// throttle state's temporary sibling) refuses the start — the bind
    /// would unlink the history at every start and every open listener
    /// would be relearned silently. The default socket, a socket in a
    /// directory that does not exist and the shipped `/run/harw` layout
    /// pass; a relative socket is refused.
    #[test]
    fn test_startup_check_refuses_a_socket_on_a_state_file() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let history = home.path().join(listener_state::DEFAULT_STATE_FILE_NAME);
        let on_history = startup_check(home.path(), &["--socket", utf8(&history)?])?;
        assert!(
            matches!(
                on_history,
                Err(export::ExportPathError::Collision {
                    first_role: "listener history",
                    second_role: "ipc socket",
                    ..
                })
            ),
            "{on_history:?}"
        );
        let throttle_temporary = home.path().join(".finding-throttle.json.tmp");
        let on_temporary = startup_check(home.path(), &["--socket", utf8(&throttle_temporary)?])?;
        assert!(
            matches!(
                on_temporary,
                Err(export::ExportPathError::SiblingOverlap {
                    first_role: "throttle state",
                    second_role: "ipc socket",
                    ..
                })
            ),
            "{on_temporary:?}"
        );
        // A symlinked parent alias of the history is refused as well.
        #[cfg(unix)]
        {
            let link = home.path().join("alias");
            std::os::unix::fs::symlink(home.path(), &link).map_err(ctx("symlink"))?;
            let aliased = link.join(listener_state::DEFAULT_STATE_FILE_NAME);
            let via_alias = startup_check(home.path(), &["--socket", utf8(&aliased)?])?;
            assert!(
                matches!(via_alias, Err(export::ExportPathError::Collision { .. })),
                "{via_alias:?}"
            );
        }

        startup_check(home.path(), &[])?.map_err(ctx("default socket"))?;
        let missing_dir = home.path().join("not-yet").join("sentinel.sock");
        startup_check(home.path(), &["--socket", utf8(&missing_dir)?])?
            .map_err(ctx("socket directory does not exist yet"))?;
        let run_dir = tempfile::tempdir().map_err(ctx("run dir"))?;
        startup_check(
            home.path(),
            &["--socket", utf8(&run_dir.path().join("sentinel.sock"))?],
        )?
        .map_err(ctx("socket in its own directory"))?;
        assert!(matches!(
            startup_check(home.path(), &["--socket", "relative.sock"])?,
            Err(export::ExportPathError::NonNormal {
                role: "ipc socket",
                ..
            })
        ));
        Ok(())
    }

    /// PL-100: `--print-sandbox` nennt die Zustandsdateien unter `--home`
    /// und die Herkunft der Sonden-Konten (Flag vor `[sentinel]` vor
    /// Vorgabe).
    #[test]
    fn test_print_sandbox_lists_the_state_files_and_the_peer_source() -> TestResult {
        let home = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home_arg = utf8(home.path())?;

        let lines = sandbox_report(&["--home", home_arg])?;
        for expected in [
            format!(
                "state-file\tthrottle\t{:?}",
                home.path().join(throttle::DEFAULT_STATE_FILE_NAME)
            ),
            format!(
                "state-file\tlistener-history\t{:?}",
                home.path().join(listener_state::DEFAULT_STATE_FILE_NAME)
            ),
            "ipc-peer-source\tbuilt-in".to_owned(),
        ] {
            assert!(lines.contains(&expected), "{expected:?} missing: {lines:?}");
        }

        let settings = settings_with_sentinel("probe_peer_users = [\"4243\"]")?;
        let lines = sandbox_report_with(&["--home", home_arg], Some(&settings))?;
        assert!(
            lines.contains(&"ipc-peer-source\tdod-config".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"ipc-peer-uid\t4243\t\"4243\"".to_owned()),
            "{lines:?}"
        );

        let lines = sandbox_report_with(
            &["--home", home_arg, "--probe-peer-user", "4244"],
            Some(&settings),
        )?;
        assert!(
            lines.contains(&"ipc-peer-source\tcommand-line".to_owned()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"ipc-peer-uid\t4244\t\"4244\"".to_owned()),
            "{lines:?}"
        );
        assert!(
            !lines
                .iter()
                .any(|line| line.starts_with("ipc-peer") && line.contains("4243")),
            "the flag replaces the configured list: {lines:?}"
        );
        Ok(())
    }

    #[test]
    fn test_export_parent_ignores_a_bare_file_name() {
        assert_eq!(
            export_parent(Path::new("/var/lib/harw-dod/export/findings.jsonl")),
            Some(Path::new("/var/lib/harw-dod/export"))
        );
        assert_eq!(export_parent(Path::new("findings.jsonl")), None);
    }

    /// Deckt die IPC-Inbox-Entleerung ab — ausschließlich über
    /// [`ipc::IpcInbox::push`] direkt befüllt, nie über einen echten Socket
    /// (harte Auflage dieses Knotens).
    #[cfg(target_os = "linux")]
    mod external_events {
        use super::*;
        use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
        use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
        use harw_types::SensorId;
        use std::path::PathBuf;

        /// Ein Mock-Sensor, dessen `poll` stets einen permanenten Fehler
        /// liefert — degradiert nach genau einem Aufruf (siehe
        /// `harw_dod_cap::SensorError::OutsideScope::permanence`) und erzeugt
        /// dabei selbst ein `EventKind::SensorDegraded`-Ereignis über
        /// `Sentinel::poll_all`, unabhängig vom hier getesteten externen Weg.
        #[derive(Debug)]
        struct AlwaysPermanentlyFailingSensor {
            handle: SensorHandle<Bound>,
        }

        impl Sensor for AlwaysPermanentlyFailingSensor {
            fn handle(&self) -> &SensorHandle<Bound> {
                &self.handle
            }

            fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
                Err(SensorError::OutsideScope)
            }
        }

        fn mock_degrading_sensor(id: &str) -> Arc<dyn Sensor> {
            let scope =
                ReadScope::from_roots([PathBuf::from("/nonexistent-harw-sentinel-mock-root")]);
            let handle =
                SensorHandle::new(SensorId::from_str(id), Capability::ReadProcStat).bind(scope);
            Arc::new(AlwaysPermanentlyFailingSensor { handle })
        }

        fn sample_probe_event(sensor_id: &str) -> SecurityEvent {
            SecurityEvent {
                sensor: SensorId::from_str(sensor_id),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind: EventKind::SensorDegraded {
                    sensor: SensorId::from_str(sensor_id),
                },
            }
        }

        /// Ein Mock-Sensor, der bei jedem Abruf dieselben Ereignisse liefert.
        #[derive(Debug)]
        struct EventEmittingSensor {
            handle: SensorHandle<Bound>,
            events: Vec<SecurityEvent>,
        }

        impl Sensor for EventEmittingSensor {
            fn handle(&self) -> &SensorHandle<Bound> {
                &self.handle
            }

            fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
                Ok(SensorReading {
                    samples: Vec::new(),
                    events: self.events.clone(),
                })
            }
        }

        /// Sammelt die formatierte `tracing`-Ausgabe eines Tests.
        #[derive(Debug, Clone, Default)]
        struct Captured(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for Captured {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0
                    .lock()
                    .map_err(|_| std::io::Error::other("capture lock poisoned"))?
                    .extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        /// PL-100 WP0.6: Ereignisinhalte (Pfade, Ziele) erreichen das Journal
        /// nie auf `warn`; der daraus entstehende Befund steht dort nur mit
        /// Kennungen.
        #[test]
        fn test_poll_once_logs_no_event_content_at_warn() -> TestResult {
            let id = "mock-events-0";
            let scope =
                ReadScope::from_roots([PathBuf::from("/nonexistent-harw-sentinel-mock-root")]);
            let handle =
                SensorHandle::new(SensorId::from_str(id), Capability::ReadProcStat).bind(scope);
            let event = |kind: EventKind| SecurityEvent {
                sensor: SensorId::from_str(id),
                observed_at: Timestamp::UNIX_EPOCH,
                actor: None,
                kind,
            };
            let sensor: Arc<dyn Sensor> = Arc::new(EventEmittingSensor {
                handle,
                events: vec![
                    event(EventKind::FileWrite {
                        path: "/etc/shadow-secret".to_owned(),
                    }),
                    event(EventKind::EgressFlow {
                        destination: "203.0.113.9".to_owned(),
                        port: 443,
                    }),
                ],
            });
            let mut sentinel = Sentinel::new(
                vec![sensor],
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            let captured = Captured::default();
            let writer = captured.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(move || writer.clone())
                .with_max_level(tracing::Level::WARN)
                .with_ansi(false)
                .finish();

            tracing::subscriber::with_default(subscriber, || {
                poll_once(
                    &mut sentinel,
                    None,
                    &harw_observe::NullSink,
                    &mut rule_cycle(),
                    None,
                    None,
                    None,
                );
            });

            assert_eq!(sentinel.buffer().event_len(), 2, "the events were recorded");
            let bytes = captured
                .0
                .lock()
                .map_err(|_| TestError::Missing("capture lock"))?
                .clone();
            let text = String::from_utf8(bytes).map_err(ctx("utf-8 log output"))?;
            assert!(text.contains("security rule finding"), "{text}");
            for leaked in ["/etc/shadow-secret", "203.0.113.9"] {
                assert!(!text.contains(leaked), "{leaked} reached warn: {text}");
            }
            Ok(())
        }

        fn sample_peer() -> ipc::PeerCredentials {
            ipc::PeerCredentials {
                pid: 4242,
                uid: 1000,
                gid: 1000,
            }
        }

        #[test]
        fn test_externally_received_ipc_event_appears_in_buffer_and_freeze() -> TestResult {
            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            inbox
                .lock()
                .map_err(ctx("uncontended lock"))?
                .push(ipc::ReceivedEvent {
                    event: sample_probe_event("probe-fs-0"),
                    peer: sample_peer(),
                });

            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            poll_once(
                &mut sentinel,
                Some(&inbox),
                &harw_observe::NullSink,
                &mut rule_cycle(),
                None,
                None,
                None,
            );

            assert_eq!(sentinel.buffer().event_len(), 1);
            let evidence = sentinel
                .freeze(Timestamp::UNIX_EPOCH)
                .map_err(ctx("well-formed buffer content always encodes"))?;
            assert_eq!(evidence.events.len(), 1);
            assert_eq!(evidence.events[0].sensor.as_str(), "probe-fs-0");
            Ok(())
        }

        #[test]
        fn test_received_raw_cgroup_id_is_resolved_beneath_the_cgroup_root() -> TestResult {
            use std::os::unix::fs::MetadataExt as _;

            let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let job = tree.path().join("harw.slice").join("harw-job-7");
            std::fs::create_dir_all(&job).map_err(ctx("fixture cgroup"))?;
            let job_id = std::fs::symlink_metadata(&job)
                .map_err(ctx("fixture metadata"))?
                .ino();
            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            {
                let mut guard = inbox.lock().map_err(ctx("uncontended lock"))?;
                for cgroup in [
                    harw_dod_signals::Actor::raw_cgroup(job_id),
                    harw_dod_signals::Actor::raw_cgroup(u64::MAX),
                ] {
                    let mut event = sample_probe_event("probe-bpf-0");
                    event.actor = Some(harw_dod_signals::Actor {
                        uid: 1000,
                        auid: None,
                        cgroup,
                        process: None,
                    });
                    guard.push(ipc::ReceivedEvent {
                        event,
                        peer: sample_peer(),
                    });
                }
            }
            let mut resolver = cgroup_resolve::CgroupResolver::new(tree.path().to_path_buf());
            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            poll_once(
                &mut sentinel,
                Some(&inbox),
                &harw_observe::NullSink,
                &mut rule_cycle(),
                None,
                None,
                Some(&mut resolver),
            );

            let cgroups: Vec<Option<String>> = sentinel
                .buffer()
                .events()
                .map(|event| {
                    event
                        .actor
                        .as_ref()
                        .and_then(|actor| actor.cgroup.as_ref())
                        .map(|id| id.as_str().to_owned())
                })
                .collect();
            assert_eq!(
                cgroups,
                vec![job.to_str().map(str::to_owned), None],
                "a resolved id carries its absolute path, an unknown id none"
            );
            Ok(())
        }

        #[test]
        fn test_drain_hands_back_the_kernel_id_of_each_resolved_cgroup() -> TestResult {
            use std::os::unix::fs::MetadataExt as _;

            let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let job = tree.path().join("harw-job-8");
            std::fs::create_dir_all(&job).map_err(ctx("fixture cgroup"))?;
            let job_id = std::fs::symlink_metadata(&job)
                .map_err(ctx("fixture metadata"))?
                .ino();
            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            {
                let mut guard = inbox.lock().map_err(ctx("uncontended lock"))?;
                let mut event = sample_probe_event("probe-bpf-0");
                event.actor = Some(harw_dod_signals::Actor {
                    uid: 1000,
                    auid: None,
                    cgroup: harw_dod_signals::Actor::raw_cgroup(job_id),
                    process: None,
                });
                guard.push(ipc::ReceivedEvent {
                    event,
                    peer: sample_peer(),
                });
            }
            let mut resolver = cgroup_resolve::CgroupResolver::new(tree.path().to_path_buf());
            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );

            let kids = drain_external_events(&mut sentinel, Some(&inbox), Some(&mut resolver));

            let path = job.to_str().ok_or(TestError::Missing("utf-8 path"))?;
            assert_eq!(kids.kid_of(path), Some(job_id));
            Ok(())
        }

        #[test]
        fn test_poll_once_drains_the_inbox_after_poll_all_so_internal_events_precede_external_ones()
        -> TestResult {
            let sensor = mock_degrading_sensor("mock-degrade-0");
            let mut sentinel = Sentinel::new(
                vec![sensor],
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );

            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            inbox
                .lock()
                .map_err(ctx("uncontended lock"))?
                .push(ipc::ReceivedEvent {
                    event: sample_probe_event("probe-fs-0"),
                    peer: sample_peer(),
                });

            poll_once(
                &mut sentinel,
                Some(&inbox),
                &harw_observe::NullSink,
                &mut rule_cycle(),
                None,
                None,
                None,
            );

            // Reihenfolge (Frage 1): das intern erzeugte `SensorDegraded`
            // aus `poll_all` steht vor dem extern eingespeisten Ereignis —
            // die `SensorId` unterscheidet dabei eindeutig Herkunft intern
            // (`mock-degrade-0`, ein bei `Sentinel::new` registrierter
            // Sensor) von Herkunft extern (`probe-fs-0`, kein registrierter
            // Sensor dieser Instanz).
            let ids: Vec<String> = sentinel
                .buffer()
                .events()
                .map(|event| event.sensor.as_str().to_owned())
                .collect();
            assert_eq!(
                ids,
                vec!["mock-degrade-0".to_owned(), "probe-fs-0".to_owned()]
            );
            assert_eq!(
                sentinel.degraded_count(),
                1,
                "the registered mock sensor degraded"
            );
            Ok(())
        }

        #[test]
        fn test_poll_once_rewrites_the_health_document_every_cycle() -> TestResult {
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let path = dir.path().join("health.json");
            let mut health = health_export::HealthExporter::new(
                path.clone(),
                health_export::ConfigIdentity::default(),
            );
            let mut sentinel = Sentinel::new(
                vec![mock_degrading_sensor("mock-degrade-0")],
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            {
                let mut guard = inbox.lock().map_err(ctx("uncontended lock"))?;
                guard.peer_connected();
                guard.push(ipc::ReceivedEvent {
                    event: sample_probe_event("probe-fs-0"),
                    peer: sample_peer(),
                });
            }

            for _ in 0..2 {
                poll_once(
                    &mut sentinel,
                    Some(&inbox),
                    &harw_observe::NullSink,
                    &mut rule_cycle(),
                    None,
                    Some(&mut health),
                    None,
                );
            }
            assert_eq!(health.written(), 2);

            let text = std::fs::read_to_string(&path).map_err(ctx("read health"))?;
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(ctx("parse health"))?;
            assert_eq!(value["schema_version"], 1);
            let sensor = &value["sensors"][0];
            assert_eq!(sensor["id"], "mock-degrade-0");
            assert_eq!(sensor["health"], "degraded");
            assert_eq!(sensor["degrade_reason"], "permanent");
            // Degradiert nach dem ersten Abruf; der zweite Zyklus ruft ihn
            // nicht mehr ab und zählt deshalb auch keinen Fehler mehr.
            assert_eq!(sensor["errors_total"], 1);
            assert_eq!(sensor["readings_total"], 0);
            assert_eq!(value["ipc"]["probes_connected"], 1);
            assert_eq!(value["ipc"]["events_total"], 1);
            assert_eq!(value["ipc"]["dropped_total"], 0);
            Ok(())
        }

        #[test]
        fn test_poll_once_with_no_bound_socket_is_a_no_op_on_the_buffer() {
            // `inbox == None` entspricht einem gescheiterten `bind()` beim
            // Start (siehe `bind_ipc_if_available`) — kein Fehler, nur kein
            // externes Ereignis in diesem Zyklus.
            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            poll_once(
                &mut sentinel,
                None,
                &harw_observe::NullSink,
                &mut rule_cycle(),
                None,
                None,
                None,
            );
            assert_eq!(sentinel.buffer().event_len(), 0);
        }

        /// PL-100 owner review (4504896422/4504911835): one production-path
        /// fixture. The IPC inbox delivers five events in one cycle into a
        /// ring of two: three are evicted before the rules see them, and
        /// the surviving egress event's raw cgroup id is resolved beneath
        /// `--cgroup-root`. The exported finding carries the cgroup's kernel
        /// id and path, and `health.json` reports the overflow.
        #[test]
        fn test_poll_once_attributes_the_cgroup_kid_and_reports_ring_overflow_in_one_cycle()
        -> TestResult {
            use std::os::unix::fs::MetadataExt as _;

            let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let job = tree.path().join("harw.slice").join("harw-job-9");
            std::fs::create_dir_all(&job).map_err(ctx("fixture cgroup"))?;
            let job_kid = std::fs::symlink_metadata(&job)
                .map_err(ctx("fixture metadata"))?
                .ino();
            let out = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let findings_path = out.path().join("findings.jsonl");
            let health_path = out.path().join("health.json");
            let host = harw_types::HostId::try_from_str("host-a").map_err(ctx("host id"))?;
            let mut exporter = export::FindingsExporter::new(findings_path.clone(), host);
            let mut health = health_export::HealthExporter::new(
                health_path.clone(),
                health_export::ConfigIdentity::default(),
            );
            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig {
                    event_capacity: 2,
                    ..SentinelConfig::default()
                },
            );
            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(8)));
            {
                let mut guard = inbox.lock().map_err(ctx("uncontended lock"))?;
                // Vier Ereignisse ohne Regeltreffer (keine Regel wertet
                // Anmeldungen aus) vor dem einen Fluss.
                for _ in 0..4 {
                    guard.push(ipc::ReceivedEvent {
                        event: SecurityEvent {
                            sensor: SensorId::from_str("probe-auth-0"),
                            observed_at: Timestamp::UNIX_EPOCH,
                            actor: None,
                            kind: EventKind::AuthEvent {
                                outcome: harw_dod_signals::AuthOutcome::Success,
                            },
                        },
                        peer: sample_peer(),
                    });
                }
                guard.push(ipc::ReceivedEvent {
                    event: SecurityEvent {
                        sensor: SensorId::from_str("probe-bpf-0"),
                        observed_at: Timestamp::UNIX_EPOCH,
                        actor: Some(harw_dod_signals::Actor {
                            uid: 1000,
                            auid: None,
                            cgroup: harw_dod_signals::Actor::raw_cgroup(job_kid),
                            process: None,
                        }),
                        kind: EventKind::EgressFlow {
                            destination: "198.51.100.5".to_owned(),
                            port: 443,
                        },
                    },
                    peer: sample_peer(),
                });
            }
            let mut resolver = cgroup_resolve::CgroupResolver::new(tree.path().to_path_buf());
            let mut rules = rule_cycle();

            poll_once(
                &mut sentinel,
                Some(&inbox),
                &harw_observe::NullSink,
                &mut rules,
                Some(&mut exporter),
                Some(&mut health),
                Some(&mut resolver),
            );

            // Attribution: Kernel-ID und Pfad der aufgelösten Cgroup.
            let job_path = job.to_str().ok_or(TestError::Missing("utf-8 path"))?;
            let text = std::fs::read_to_string(&findings_path).map_err(ctx("read export"))?;
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 1, "exactly the egress finding: {text}");
            let line = lines.first().ok_or(TestError::Missing("export line"))?;
            let parsed = harw_dod_client::findings::parse_finding_line(line)
                .ok_or(TestError::Missing("a line harw-dod-client accepts"))?;
            assert_eq!(parsed.rule_id, "egress-flow");
            assert_eq!(parsed.cgroup_kid, Some(job_kid));
            assert_eq!(parsed.scope.as_deref(), Some(job_path));

            // Überlauf: drei Ereignisse verdrängt, bevor die Regeln sie sahen.
            assert_eq!(rules.ring_counters().overflow_events_total, 3);
            let document: serde_json::Value = serde_json::from_str(
                &std::fs::read_to_string(&health_path).map_err(ctx("read health"))?,
            )
            .map_err(ctx("parse health"))?;
            let counter = |object: &str, field: &str| {
                document
                    .get(object)
                    .and_then(|value| value.get(field))
                    .and_then(serde_json::Value::as_u64)
            };
            assert_eq!(counter("ring", "overflow_events_total"), Some(3));
            assert_eq!(counter("ring", "overflow_events_last_cycle"), Some(3));
            assert_eq!(counter("ring", "overflow_samples_total"), Some(0));
            assert_eq!(counter("ipc", "events_total"), Some(5));
            assert_eq!(counter("ipc", "dropped_total"), Some(0));
            Ok(())
        }

        /// Erfasst gemeldete Messwerte (Name und Wert) für die Metrik-Tests.
        #[derive(Debug, Default)]
        struct MetricCapture(Mutex<Vec<(&'static str, harw_observe::MetricValue)>>);

        impl MetricCapture {
            /// Die gemeldeten Zählerstände der Serie `name`, in Reihenfolge.
            fn counts(&self, name: &str) -> TestResult<Vec<u64>> {
                let seen = self.0.lock().map_err(|_| TestError::Missing("capture"))?;
                Ok(seen
                    .iter()
                    .filter(|(key, _)| *key == name)
                    .filter_map(|(_, value)| match value {
                        harw_observe::MetricValue::Count(count) => Some(*count),
                        harw_observe::MetricValue::Gauge(_)
                        | harw_observe::MetricValue::Observation(_) => None,
                    })
                    .collect())
            }
        }

        impl TelemetrySink for MetricCapture {
            fn record(
                &self,
                key: &harw_observe::MetricKey,
                value: harw_observe::MetricValue,
                _labels: &[(harw_observe::FieldName, harw_observe::FieldValue)],
            ) {
                // Ein vergifteter Mutex lässt den Test über `counts` scheitern.
                if let Ok(mut seen) = self.0.lock() {
                    seen.push((key.name, value));
                }
            }

            fn flush(&self) {}

            fn name(&self) -> &'static str {
                "capture"
            }
        }

        /// PL-100: verworfene IPC-Nachrichten erreichen
        /// `sensor_parse_errors_total` über das installierte
        /// `SensorMetrics`-Handle — die Nullserie bei der Installation, danach
        /// je Zyklus genau ein absoluter Stand für den Block.
        #[test]
        fn test_report_ipc_parse_errors_publishes_the_block_once_per_cycle() -> TestResult {
            let capture = Arc::new(MetricCapture::default());
            let file_sink: Arc<dyn TelemetrySink> = Arc::clone(&capture) as Arc<dyn TelemetrySink>;
            let (_sink, metrics) = install_sensor_metrics(file_sink);
            let metrics = metrics.ok_or(TestError::Missing("sensor metrics"))?;
            assert_eq!(capture.counts("sensor_parse_errors_total")?, vec![0]);

            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            {
                let mut guard = inbox.lock().map_err(ctx("uncontended lock"))?;
                guard.note_decode_error();
                guard.note_decode_error();
            }
            assert_eq!(report_ipc_parse_errors(Some(&inbox), Some(&metrics)), 2);
            assert_eq!(
                report_ipc_parse_errors(Some(&inbox), Some(&metrics)),
                0,
                "a reported block is not reported again"
            );
            inbox
                .lock()
                .map_err(ctx("uncontended lock"))?
                .note_decode_error();
            assert_eq!(report_ipc_parse_errors(Some(&inbox), Some(&metrics)), 1);
            assert_eq!(report_ipc_parse_errors(None, Some(&metrics)), 0);
            assert_eq!(
                capture.counts("sensor_parse_errors_total")?,
                vec![0, 2, 3],
                "absolute totals, one delivery per non-empty block"
            );
            // Review R2: der kumulierte Stand erscheint unabhängig vom
            // Melden des Blocks auch in `health.json` (`ipc.decode_errors_total`).
            assert_eq!(ipc_counters(Some(&inbox)).decode_errors_total, 3);
            assert_eq!(ipc_counters(None).decode_errors_total, 0);
            Ok(())
        }

        /// Review R2: verworfene Sondennachrichten erreichen über `poll_once`
        /// das Feld `ipc.decode_errors_total` von `health.json`, und
        /// `harw-dod-client` liest es.
        #[test]
        fn test_poll_once_reports_ipc_decode_errors_in_health_for_the_client() -> TestResult {
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let path = dir.path().join(health_export::DEFAULT_HEALTH_FILE_NAME);
            let mut health = health_export::HealthExporter::new(
                path.clone(),
                health_export::ConfigIdentity::default(),
            );
            let mut sentinel = Sentinel::new(
                Vec::new(),
                Arc::new(harw_observe::NullSink),
                SentinelConfig::default(),
            );
            let inbox: IpcInboxHandle = Arc::new(Mutex::new(ipc::IpcInbox::new(4)));
            {
                let mut guard = inbox.lock().map_err(ctx("uncontended lock"))?;
                guard.note_decode_error();
                guard.note_decode_error();
            }
            let mut rules = rule_cycle();

            poll_once(
                &mut sentinel,
                Some(&inbox),
                &harw_observe::NullSink,
                &mut rules,
                None,
                Some(&mut health),
                None,
            );

            let report = harw_dod_client::health::read_health(&path)
                .map_err(ctx("harw-dod-client reads the health document"))?
                .ok_or(TestError::Missing("health document"))?;
            let ipc = report.ipc.ok_or(TestError::Missing("ipc counters"))?;
            assert_eq!(ipc.decode_errors_total, 2);
            assert_eq!(ipc.events_total, 0, "nothing decodable arrived");
            Ok(())
        }

        /// PL-100 owner review (4505015296), composition order: binding the
        /// socket (before Landlock) starts no thread — a probe that connects
        /// and sends right away stays in the socket backlog — and only
        /// `start_receiving`, which needs the Landlock proof, accepts and
        /// reads it.
        #[test]
        fn test_bound_ipc_receives_nothing_until_started_with_the_sandbox_proof() -> TestResult {
            use rustix::net::{AddressFamily, SendFlags, SocketAddrUnix, SocketType};
            use std::os::unix::fs::MetadataExt as _;

            let dir = tempfile::Builder::new()
                .prefix("hs-ord")
                .tempdir_in("/tmp")
                .map_err(ctx("tempdir"))?;
            let own_uid = std::fs::metadata(dir.path())
                .map_err(ctx("tempdir metadata"))?
                .uid();
            let socket = dir.path().join("s.sock");
            let bound = bind_ipc_if_available(&socket).ok_or(TestError::Missing("bound socket"))?;

            let event = sample_probe_event("probe-fs-0");
            let payload = serde_json::to_vec(&event).map_err(ctx("encode event"))?;
            let client = rustix::net::socket(AddressFamily::UNIX, SocketType::SEQPACKET, None)
                .map_err(ctx("client socket"))?;
            let address = SocketAddrUnix::new(socket.as_path()).map_err(ctx("client address"))?;
            rustix::net::connect(&client, &address).map_err(ctx("connect"))?;
            rustix::net::send(&client, &payload, SendFlags::empty()).map_err(ctx("send"))?;

            // Vor `start_receiving`: kein Annahme-, kein Empfangsthread.
            std::thread::sleep(Duration::from_millis(200));
            {
                let guard = bound.inbox.lock().map_err(ctx("inbox lock"))?;
                assert_eq!(guard.received_total(), 0, "nothing read before the sandbox");
                assert_eq!(guard.connected(), 0, "nothing accepted before the sandbox");
            }

            let proof =
                sandbox::SandboxApplied::assumed_for_test(sandbox::SandboxOutcome::FullyEnforced);
            let inbox =
                bound.start_receiving(peer_allow::PeerAllowlist::with_root([own_uid]), &proof);

            let deadline = std::time::Instant::now()
                .checked_add(Duration::from_secs(5))
                .ok_or(TestError::Missing("deadline"))?;
            loop {
                let received = inbox.lock().map_err(ctx("inbox lock"))?.received_total();
                if received >= 1 {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(TestError::Unexpected(
                        "the started receive thread did not read the backlog in 5 s".to_owned(),
                    ));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let drained = inbox.lock().map_err(ctx("inbox lock"))?.drain();
            let received = drained
                .first()
                .ok_or(TestError::Missing("received event"))?;
            assert_eq!(received.event, event);
            assert_eq!(received.peer.uid, own_uid);
            drop(client);
            Ok(())
        }
    }
}

#[cfg(test)]
mod telemetry_max_files_tests {
    use super::telemetry_max_files;

    #[test]
    fn test_flag_overrides_class_default() {
        assert_eq!(telemetry_max_files(Some(3)), Some(3));
        assert_eq!(telemetry_max_files(None), Some(20));
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
