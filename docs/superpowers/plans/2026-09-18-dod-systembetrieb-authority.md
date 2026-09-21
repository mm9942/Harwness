# DoD-Systembetrieb, eBPF und Authority-Kern — Umsetzungsplan

Stand: 18. September 2026. Status: zur Umsetzung vorbereiteter Plan; keine der folgenden Änderungen ist durch dieses Dokument bereits implementiert oder am Host verifiziert.

## 1. Ziel und verbindliche Entscheidungen

DoD soll auf dem lokalen Linux-Rechner als eigenständige Systemsoftware mit echter eBPF-Beobachtung laufen. Installation, Konfiguration, Programme und Dienstdefinitionen gehören root. Dedizierte Systemkonten besitzen ausschließlich die jeweils erforderlichen Betriebsdaten. Der persönliche HARW-Home-Baum ist kein Installationsziel.

Parallel dazu wird das bisherige Rechte-Modell vollständig auf einen grundständigen `harw-authority`-Kern umgestellt. Es gibt keine Kompatibilitäts-Fassade für die alte Rechtekonstruktion. `harw-macros` vereinfacht Deklarationen und Verdrahtung; die tatsächliche Autorisierung bleibt im Authority-Kern.

Verbindlich aus der Abstimmung:

- Eigene `dod/Makefile`, einschließlich Installation sämtlicher DoD-Laufzeitbestandteile und systemd-Service-Dateien.
- Systemweite systemd-Dienste auf diesem Rechner; keine User-Units und keine File-Capabilities auf persönlich beschreibbaren Binaries.
- Produktiver eBPF-Pfad einschließlich versionierter Quellen und reproduzierbarem ELF-Build.
- TOML-Profile für verschiedene Gruppen von cgroups sowie den ganzen Host. Host-Beobachtung erfolgt ausschließlich nach ausdrücklicher Profilwahl.
- Zunächst Beobachtung. Warden wird installiert, bleibt einschließlich Socket deaktiviert. Freeze, Kill und Netzwerkisolation werden nicht aktiviert.
- `doctor` prüft Voraussetzungen und erklärt fehlende Installationsschritte. Reguläre Build- und Install-Targets installieren keine Toolchains oder Betriebssystempakete automatisch.
- Breaking Changes an internen APIs sind erlaubt; sämtliche betroffenen Aufrufer werden migriert.
- Bestehende Arbeitskopieänderungen bleiben erhalten. Insbesondere Runtime, Registry, Config und Macros enthalten bereits fremde Änderungen.

Die Arbeit wird in zwei separat abnehmbare Lieferungen geteilt: A = DoD-Systembetrieb; B = Authority-Umbau. A ist nicht vom Abschluss von B abhängig. Nach B müssen beide Cargo-Workspaces erneut geprüft werden.

## 2. Ausgangslage und korrigierte Annahmen

DoD besitzt bereits einen eigenen Cargo-Workspace unter `dod/`. `harw-sentinel` sammelt Sensorwerte, verarbeitet externe Ereignisse und ruft Regeln auf. `harw-probe-bpf` verbindet sich push-only über einen Unix-SEQPACKET-Socket. `harw-dod-bpf/src/real.rs` enthält bereits einen Aya-basierten `RealBpfLoader`; die frühere Aussage, es gebe keinen echten Loader, war falsch. Es fehlen die ausgelieferten Kernelprogramme und ein nachgewiesener Produktionsbetrieb.

Der bisherige Loader erwartet ein Programm pro ELF und eine Ringbuffer-Map `EVENTS`. Die Probe besitzt bereits Argumente für zwei Programmpfade und erlaubte CIDRs. Einige Modulkommentare beschreiben noch einen älteren Platzhalterstand und müssen mit dem implementierten Verhalten abgeglichen werden.

`harw-warden` besitzt eine cgroup-Ausführungsschicht und systemd-Socket-Aktivierung. Der Netzwerk-Isolator ist nicht implementiert. Ein erfolgreicher Beobachtungsbetrieb ist deshalb kein Nachweis für Netzwerkdurchsetzung.

`PermissionSet::from_policy` ist öffentlich; zusätzlich ist `PermissionSet` deserialisierbar. Viele Haupt-Workspace-Aufrufer erzeugen damit eigentlich nur Anforderungen oder Obergrenzen, andere echte Root-Rechte und viele Testfixtures. Die DoD-Quellen verwenden diesen Konstruktor nach der bisherigen Suche nicht direkt. Sie verwenden insbesondere `NetworkScope` aus `harw-sandbox`.

Die bisherige Behauptung, ein neuer Crate oder private Konstruktoren allein machten das gesamte System mathematisch geschlossen, wird nicht übernommen. Ein öffentlicher Policy-Compiler über frei konstruierbare Eingaben wäre weiterhin ein Aussteller beliebiger Rechte. Root-Ausstellung muss auf einer überprüften Vertrauensquelle beruhen. Ebenso schützen Rust-Typen nicht vor beliebigem bösartigem nativen Code mit denselben Betriebssystemrechten.

Die lokale Vorprüfung zeigte aarch64 und einen Raspberry-Pi-Kernel 6.18 sowie eine Rust-Beta-Toolchain. Im damaligen PATH fehlten clang und bpftool. Diese Momentaufnahme ersetzt keinen `doctor`-Lauf. Insbesondere ist `rustup target add bpfel-unknown-none` kein vollständiger eBPF-Buildweg: der Rust-BPF-Build benötigt eine passende gepinnte Toolchain, rust-src, build-std und einen BPF-Linker.

## 3. Lieferung A: Konfiguration und Profile

### 3.1 Ein Systemvertrag

Neue kleine DoD-Crate `harw-dod-config`: Parsen, Validierung und Auflösung der Systemkonfiguration. Keine Abhängigkeit auf `harw-runtime`, Registry oder Tool-Crates. Alle beteiligten Dienste verwenden denselben Vertrag.

Produktionsquelle: `/etc/harw-dod/config.toml`. Keine automatische Suche im aktuellen Repository, Benutzer-Home oder über `HARW_HOME`. Ein alternativer `--config`-Pfad dient expliziten Tests und administrativen Starts. Privilegierte Starts prüfen Eigentümer, Schreibrechte und die Pfadkette; symlinkbasierte oder zwischen Prüfung und Lesen austauschbare Konfiguration darf die Vertrauensprüfung nicht umgehen.

Beispiel des auszuliefernden Schemas:

```toml
schema_version = 1
mode = "observe"
# Keine implizite aktive Auswahl. Vor enable ausdrücklich ergänzen:
# active_profile = "selected-services"

[profiles.selected-services]
scope = "cgroups"
cgroup_paths = ["/system.slice/example-a.service", "/system.slice/example-b.service"]
include_descendants = true
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []

[profiles.user-workloads]
scope = "cgroups"
cgroup_paths = ["/user.slice"]
include_descendants = true
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []

[profiles.host]
scope = "host"
sensors = ["exec", "tcp-connect"]
egress_allow_cidrs = []
```

Die Beispiele sind Vorlagen; `example-a.service` wird nicht als reale lokale Unit behauptet. Betreiber können weitere benannte Profile mit mehreren cgroups definieren. Genau ein Profil ist aktiv. Mehrere Bereiche werden durch dessen Liste zusammengefasst.

Regeln:

- Fehlendes aktives Profil: Installation erlaubt, Aktivierung verweigert mit verständlicher Meldung.
- Unbekannte Felder, Profilnamen, Sensoren, ungültige CIDRs oder widersprüchliche host/cgroup-Angaben: Fehler ohne Fallback.
- cgroup-Pfade sind relativ zur cgroup-v2-Mountwurzel dargestellt, beginnen mit `/`, enthalten keine Traversierung und sind keine bloßen Stringpräfixe. `/a` umfasst nicht `/ab`.
- `scope = "host"` muss ausdrücklich gewählt werden; eine leere cgroup-Liste bedeutet niemals Host.
- Nicht vorhandene ausgewählte cgroups verhindern einen vollständigen Start. Verschwindende oder ersetzte cgroups im Betrieb führen zu sichtbarer Degradation und gesperrter Erfassung für diesen Bereich, niemals zu Erweiterung.
- Auswahl und Erfassung betreffen Prozess-/Flow-Daten. Hostweite CPU-/Temperaturmessungen sind gesonderte Basismetriken und werden im Status als solche ausgewiesen. Listener-/Workspace-Sensoren dürfen in eingeschränkten Profilen nicht heimlich zusätzliche Prozess- oder Dateidaten sammeln; sie bleiben dort aus, bis sie denselben Scope zuverlässig beachten.
- Leere Egress-Allowlist: alle erfassten ausgehenden TCP-Ziele liegen außerhalb der erlaubten Menge. Die Liste bewertet Ereignisse; sie schaltet keine Firewall frei.
- Profilwechsel erfolgt über validierte Konfiguration und geordneten Neustart. Kein Hot-Reload in der ersten Lieferung.

Die bisherige `[dod]`-Eskalationssektion im Produkt bleibt ein separater Vertrag. Sie wird nicht stillschweigend als systemweite Beobachtungskonfiguration interpretiert. Repository-Layer dürfen keine Host-Beobachtung oder privilegierten Dienststart freischalten.

### 3.2 Gemeinsame Laufzeitentscheidung

Konfiguration wird vor Laden der Programme in einen unveränderlichen `ResolvedObservationProfile` übersetzt: Profilkennung, Scope, aufgelöste cgroup-Identitäten, Sensorliste, CIDRs und Konfigurationsdigest. Sentinel und Probe protokollieren denselben Digest. Abweichungen verhindern die Bereitschaftsmeldung der vollständigen Kette.

Die Probe filtert vor Ausgabe personenbezogener Ereignisse in den Ringbuffer. Nachgelagerte Prüfung bleibt zusätzliche Absicherung. Eine reine Filterung im Sentinel erfüllt die Beobachtungsbegrenzung nicht.

## 4. Lieferung A: eBPF und Ereigniskette

### 4.1 Builddomäne

Eigenständiges `dod/bpf/` mit eigener Toolchain-Datei und Lockfile, getrennt vom Host-Workspace. Rust/no_std und Aya-eBPF; keine Nightly-Pflicht für normale Host-Crates. Ein isolierter Buildschritt erzeugt Exec- und TCP-Objekt. Versionen von Toolchain, aya-ebpf und bpf-linker werden im ersten Build-Arbeitspaket gemeinsam auf dem Ziel validiert und exakt gepinnt, statt aus veralteten Kommentaren übernommen.

Hostcode behält sein `unsafe_code = forbid`. Falls Kernelzugriffe im BPF-Quellcrate unsafe benötigen, liegen sie ausschließlich in dieser separaten Builddomäne, mit kleinen dokumentierten Zugriffsfunktionen und Verifier-Abnahme. Diese Ausnahme darf nicht auf Host-Crates übertragen werden.

Artefakte erhalten Manifest mit Objektname, SHA-256, ABI-Version, Architektur und Toolchain-Version. Installierte Dateien sind root-eigen und nicht durch Dienstkonten beschreibbar. ELF- und Konfigurationsprüfung muss die tatsächlich anschließend geladenen Bytes prüfen; kein erneutes ungesichertes Öffnen nach Hashprüfung.

### 4.2 Ereignissemantik

Exec: erfolgreicher Prozessstart über `sched_process_exec`. Kein ungeprüftes Lesen roher Tracepoint-Offsets über verschiedene Kernel. Layoutzugriffe werden aus validiertem Kernelwissen erzeugt bzw. geprüft; ein nicht unterstütztes Layout führt zum Startfehler.

TCP: erste Lieferung erfasst ausgehende TCP-Verbindungsversuche für IPv4 und IPv6. UDP und vollständige Paketbeobachtung werden nicht als abgedeckt ausgegeben. Der bestehende Socket-State-Tracepoint allein bietet keine verlässliche Zuordnung zu aktuellem PID/UID/cgroup. Deshalb wird der bestehende Flow-Pfad auf einen Connect-Hook im aufrufenden Prozesskontext umgestellt, dessen ABI für den Zielkernel geprüft wird; der Loader erhält dafür explizite Programmauswahl und Attach-Spezifikation. Bei fehlender Unterstützung startet dieser Sensor nicht. Es gibt keinen Fallback auf vermeintliche Verursacher aus Softirq-Kontext.

Scope-Prüfung verwendet die cgroup des verursachenden Tasks einschließlich der gewählten Vorfahrensemantik. Identitäten werden stabil gehalten; Löschung/Neuanlage eines gleichnamigen Pfades darf alte IDs nicht unbemerkt weiterverwenden. Tests müssen Geschwister-, Kind-, Migrations- und Wiederanlagefälle abdecken.

Der gemeinsame BPF-Wirevertrag wird als explizit versioniertes Byteformat geführt: Typ, Länge, monotone Kernelzeit, Prozess-/cgroup-Identität und typabhängige Nutzlast. Kein implizites Rust/C-Padding. Unbekannte Versionen und widersprüchliche Längen werden verworfen und gezählt. Die bisherigen Fixtureformate werden dabei gezielt migriert; Ringbufferdaten sind kein persistentes Kompatibilitätsformat.

Zeitkonvertierung geschieht im Loader: monotone Kernelzeit plus gemessene Zuordnung zur Echtzeit. `ktime` darf nicht als Unix-Epoche interpretiert werden. Zeitsprünge und Suspend werden getestet und als Unsicherheit sichtbar gemacht.

Exec-Metadaten enthalten PID, PPID, UID, begrenzten Programmpfad und explizite Abschneideinformation. Argumente und Umgebungsvariablen werden in v1 nicht erfasst. Ein bisher erwarteter `argv_digest` wird optional mit Status „nicht erhoben“; der Hash eines leeren Puffers darf nicht als echter Argumentnachweis erscheinen. TCP trägt Zieladresse und Port, keine Paketnutzlast.

### 4.3 Loader, Landlock und Betrieb

- Bestehenden Aya-Loader weiterverwenden; Programmname, Typ, Maps und ABI prüfen, statt das erste beliebige Programm zu nehmen.
- Profilmaps vollständig befüllen, bevor Hooks aktiviert werden. Teilfehler lösen alle bereits angehängten Programme.
- Erforderliche BPF-/Tracing-Capabilities am tatsächlichen Attach-Pfad feststellen. `CAP_BPF` allein ist keine belegte Zusage; insbesondere `CAP_PERFMON` prüfen. Kein automatischer Rückfall auf `CAP_SYS_ADMIN` oder uneingeschränktes root.
- Landlock-Startreihenfolge mit realen Zugriffen abstimmen: Konfiguration, Objekte, `/proc/self/status`, Tracefs/BTF und cgroup-Auflösung. Die heutige Beschränkung auf Objekt-Elternverzeichnisse ist dafür nicht als ausreichend anzunehmen.
- Sink bleibt push-only. Sentinel authentifiziert Peer-UID und zulässige Sensor-IDs vor Übernahme. Begrenzte Verbindungen, Nachrichtengrößen, Queuekapazität und Zeitlimits.
- Ringbuffer-Verluste, IPC-Verluste und ungültige Ereignisse werden gezählt. Sensor-Heartbeat und letzter erfolgreicher Empfang unterscheiden eine ruhige Quelle von einer ausgefallenen Probe.
- Sentinel-Neustart führt zu kontrolliertem Probe-Neustart und erneutem Attach; Stop entfernt alle Links. Kein unbegrenztes Blockieren beim Senden.
- CIDR-Bewertung aus derselben Konfiguration in Probe/Regelwerk; keine versehentliche Zweitbewertung gegen den bisher fest leeren Sentinel-Scope.

## 5. Lieferung A: FHS, Dienste und Makefile

### 5.1 Installationslayout

| Pfad | Inhalt und Eigentum |
|---|---|
| `/usr/local/libexec/harw-dod/` | Programme, root:root, 0755 |
| `/usr/local/lib/harw-dod/bpf/` | Objekte und Manifest, root:root, 0644 |
| `/usr/local/lib/systemd/system/` | System-Units, root:root, 0644 |
| `/etc/harw-dod/` | Systemkonfiguration, root und lesende Dienstgruppe; nicht gruppenschreibbar |
| `/var/lib/harw-dod/` | Persistente Belege, nur Sentinel schreibberechtigt |
| `/var/log/harw-dod/` | Rotierende Telemetrie, nur Sentinel schreibberechtigt |
| `/run/harw-dod/` | Flüchtige Sockets/Laufzeitdaten mit enger Gruppenberechtigung |

Konfigurationsdatei 0640, Betriebsverzeichnisse 0750, Socket 0660. Systemkonten `harw-dod` und `harw-dod-bpf` ohne Login und ohne persönliches Home; gemeinsame IPC-Gruppe nur für den erforderlichen Socketzugriff. Warden erhält keine Probe-Identität.

Sentinel erhält getrennte Optionen für State-, Telemetrie- und Runtime-Verzeichnis. Der bisherige gemeinsame `--home` darf die Systeminstallation nicht wieder in einen persönlichen HARW-Baum lenken.

Installation unterstützt `PREFIX`, `LIBEXECDIR`, `LIBDIR`, `SYSCONFDIR`, `LOCALSTATEDIR`, `SYSTEMD_UNITDIR` und `DESTDIR`. Standard ist lokale Administratorinstallation unter `/usr/local`; Distributionen können `/usr` setzen. Runtime- und Datenpfade bleiben FHS-konform. Renderte Units enthalten Zielpfade ohne DESTDIR-Präfix.

### 5.2 systemd-Vertrag

Units: `harw-dod.target`, `harw-dod-sentinel.service`, `harw-dod-bpf.service`, `harw-dod-warden.service`, `harw-dod-warden.socket`. Das Beobachtungs-Target zieht nur Sentinel und BPF-Probe hoch. Warden besitzt keine Aktivierungskante vom Beobachtungs-Target und wird frisch installiert weder enabled noch gestartet.

Sentinel läuft ohne erhöhte Capabilities. BPF läuft unter eigenem Systemkonto mit genau den verifizierten BPF-/Tracing-Capabilities in Bounding- und Ambient-Set. `NoNewPrivileges`, schreibgeschützte Systempfade und begrenzte Schreibpfade werden mit den tatsächlich benötigten Kernelzugriffen getestet. Keine pauschale Hardening-Option übernehmen, die die BPF-Quelle oder ihren Scope unsichtbar macht.

Runtime-/State-/Logs-Verzeichnisse werden über systemd-Verzeichnisverwaltung bzw. die installierten tmpfiles/sysusers-Regeln hergestellt. Die Probe erhält keine Schreibrechte auf Konfiguration, ELF-Dateien oder Sentinel-Belege. Restart-Limits verhindern Endlosschleifen bei ungültiger Konfiguration. Bereitschaft erfordert funktionierenden IPC-Empfang und erfolgreich angehängte konfigurierte Sensoren, nicht nur einen lebenden Prozess.

### 5.3 Targets

| Target | Verbindliches Verhalten |
|---|---|
| `help` | Befehle, Privilegien und Pfadvariablen erklären |
| `doctor` | Kernel, cgroup v2, BTF/Tracepoints, Landlock, systemd, Toolchain und Linker prüfen; keine Installation |
| `check-config` | TOML und aktive Profilauflösung prüfen, effektiven Scope anzeigen |
| `build-bpf` | Beide gepinnten BPF-Objekte und Manifest bauen |
| `build` | Host-Release-Binaries plus BPF-Artefakte bauen |
| `check`, `fmt`, `clippy`, `test` | DoD-Workspace prüfen; fmt prüft ohne Umschreiben |
| `verify` | Lints, Unit-/Integrationstests und ELF-Vertragsprüfung bündeln |
| `install` | Binaries, ELFs, Konfigurationsvorlage, alle Units, sysusers/tmpfiles-Regeln installieren |
| `enable` | Konfiguration prüfen, Beobachtungs-Target aktivieren und starten |
| `disable` | Beobachtungs-Target und seine Dienste stoppen und Boot-Aktivierung entfernen |
| `restart` | Konfiguration prüfen und Beobachtungskette geordnet neu starten |
| `status`, `logs` | Dienste, aktives Profil, Sensorzustand und relevante Logs anzeigen |
| `smoke` | Expliziter Live-Test mit temporären Testprozessen/cgroups; niemals Teil normaler Tests |
| `uninstall` | Nur manifestierte DoD-Programm-/Unit-Dateien entfernen; Konfiguration und Belege erhalten |

`install` enthält Service-Dateien immer. Bei direkter Systeminstallation legt es Konten/Verzeichnisse an und führt daemon-reload aus; es startet keine Beobachtung ohne gewähltes Profil. Bei `DESTDIR` erfolgt ausschließlich Staging, ohne Kontenanlage, daemon-reload, Aktivierung oder Laufzeiteingriffe. Vorhandene Konfiguration bleibt unverändert; neue Beispiele werden separat abgelegt.

Build läuft unprivilegiert. Systemkopie und Dienstaktionen benötigen explizite Administratorrechte. `install` bei fehlenden Artefakten liefert die notwendige Build-Anweisung statt Cargo als root auszuführen. Abhängigkeiten und Toolchains werden durch doctor dokumentiert, nicht versteckt nachinstalliert.

Upgrade: Artefakte vollständig vorab prüfen, Dienste geordnet stoppen, zusammenpassende Version installieren, daemon-reload, vorher aktiven Beobachtungszustand nach erneuter Konfigurationsprüfung wiederherstellen. Frühere Version für Rollback aufbewahren. Ein Upgrade aktiviert keinen bislang deaktivierten Warden. Rollback erhält Belege und Konfiguration und prüft deren Versionsverträglichkeit.

## 6. Lieferung B: Authority-Kern ohne Fassade

### 6.1 Eigentum und Typtrennung

Neue grundständige Crate `harw-authority` im Produkt-Workspace, von DoD über Pfadabhängigkeit nutzbar. Abhängigkeiten nur auf notwendige Grundtypen/Serialisierung, keine Runtime-, Core-, Tool- oder Registry-Kanten.

Der Kern übernimmt `Permission`, `PermissionSet`, `SandboxSpec` sowie die zur identischen Scope-Auswertung erforderlichen Workspace-/Netzbereichstypen. `harw-sandbox` behält Betriebssystem-Backends wie bwrap und verwendet die Authority-Typen. Keine alten Re-Exports zur Verschleierung des Umzugs; Importe und Manifeste werden vollständig migriert. Netzwerksemantik darf dabei nicht doppelt implementiert werden.

Zentrale Trennung:

- `PermissionRequest`: frei konstruierbare, serialisierbare Anforderungen/Obergrenzen; vermittelt selbst keinerlei Ausführungsrecht.
- `PermissionSet`: gewährte Rechte mit privaten Feldern und privater Rohkonstruktion. Kein öffentliches Deserialize, FromIterator, Default mit Rechten oder unbeschränkter Builder.
- `AuthorityContext`: gewährter Kontext mit Workspace, Netzscope und Herkunft; nach Ausstellung unveränderlich.
- `AuthoritySnapshot`: serialisierbare Anzeige-/Persistenzdaten, ausdrücklich kein Grant. Wiederaufnahme benötigt erneute Policy-Auswertung.

Öffentliche Ableitung `parent.restrict(request)` liefert nur eine Teilmenge des Elternkontexts. Modus-, Rollen-, Profil- und Vertragsobergrenzen werden `PermissionRequest`; `required_permissions()` liefert Anforderungen. Sie dürfen nicht als fertige Grants in eine Sandbox gelangen.

### 6.2 Root-Vertrauensgrenze

Root-Ausstellung bleibt ein ausdrücklich dokumentierter Bootstrap-Vorgang. Der Authority-Kern liest und validiert die vertrauenswürdige Betreiberpolicy selbst; ein frei konstruierbares `TrustedPolicy` oder ein öffentliches `Issuer::new(all_permissions)` ist ausgeschlossen. Eine übergebene Anforderung kann lediglich gegen diese Policy ausgewertet werden.

Für DoD liegt die Vertrauensquelle in root-kontrollierter Systemkonfiguration. Für das benutzerbetriebene HARW bleibt die bestehende Betreiber-/Home-Policy dessen Vertrauensquelle; root-Eigentum wird dort nicht neu vorgeschrieben. Beide Betriebsarten werden getrennt typisiert. Repository-Inhalte dürfen nur zusätzliche Einschränkungen liefern und keine Vertrauensklasse selbst behaupten.

Ausstellungsentscheidungen binden den Grant an Workspace/Betreiber-Kontext und protokollieren Herkunft und Policy-Digest. Ein Root-Bootstrap-API ist damit keine Berechtigungsgrenze gegen bösartigen Code mit denselben OS-Rechten. Der nachweisbare Schutz lautet: unvalidierte Daten und gewöhnliche Konsumenten-APIs können keinen beliebigen Grant konstruieren; Ableitung aus bestehender Autorität erweitert sie nicht. Für stärkere Isolation wären getrennte Prozesse erforderlich und eine eigene Erweiterung zu planen.

Serialisierung und Wiederaufnahme werden vollständig inventarisiert. Bestehende gespeicherte Rechte werden als Snapshot/Anforderung gelesen und gegen aktuelle Root- und Elternpolicy geschnitten. Kein implizites Vertrauen in alte JSON-Rechtesätze. Fehlende Herkunft oder Policy führt zu Ablehnung bzw. einem ausdrücklich leeren Kontext, niemals zu Full-Rechten.

### 6.3 Macros

Bestehende `tool`-/`operation`-Makros verwenden die neuen Authority-Typen und generieren Guard-Aufrufe. Ergänzung `permission_request!` für validierte deklarative Permissionlisten; Ergebnis ist ausschließlich ein `PermissionRequest`. Unbekannte Einträge sind Compile-Fehler.

Keine Makroausnahme für private Grant-Konstruktion, kein `__private`-Minting-Export und keine Caller-Crate-Namensprüfung als vermeintliche Sicherheitsgrenze. `harw-macros` hält weiterhin keine produktive Laufzeitabhängigkeit auf den Authority-Kern; erzeugte absolute Pfade werden in Konsumenten aufgelöst. Handgeschriebene und generierte Guards rufen dieselbe Kernimplementierung auf.

### 6.4 Migration

Call-Sites nach Root-Ausstellung, Reduktion, Anforderungsprüfung, Snapshot und Testfixture klassifizieren. Dann gemeinsam migrieren: Runtime-Assembly, Einstiegstiers, Moduswechsel, Child-Admission, Registry-Rollen, Definition-Tools, Job-Verträge, Channel-Restriktionen und Tool-Kontexte.

Fixtures erzeugen legitime Grants über isolierte Testpolicy und denselben Validator. Beliebige Bitmengen für Algebra-Tests bleiben interne Tests des Kerns. Kein öffentliches test-support-Feature, das durch Cargo-Feature-Vereinigung einen produktiven Minting-Pfad öffnet.

Aktuell vorhandene 7 Permissionvarianten werden vollständig geprüft: alle 128 Elternmengen gegen alle 128 Anforderungen. Zusätzlich Netzwerk-/Workspace-Begrenzung und Wiederaufnahme testen. Neue Permissionvarianten müssen exhaustive Zuordnungen aktualisieren und dürfen nicht automatisch freigeschaltet werden.

## 7. Arbeitspakete und Reihenfolge

Jedes Paket liefert Code, relevante Tests und aktualisierte Betriebsdokumentation. Erst nach bestandener eigener Abnahme folgt das nächste abhängige Paket. Keine automatischen Commits fremder Änderungen.

1. **Bestandsaufnahme und Baseline:** Arbeitskopieänderungen erfassen; beide Workspace-Manifeste und existierende Gates prüfen; reale Buildfehler von Aufgabenänderungen trennen. Lokalen Kernel-/Toolchain-Bericht erstellen. Ergebnis: reproduzierbare Baseline und konkrete Voraussetzungen.
2. **DoD-Konfigurationsvertrag:** Config-Crate, Profile, Validierung und gemeinsame Auflösung implementieren; Negativfälle zuerst testen. Ergebnis: effektiver Scope ist ohne Kernelmutationen prüfbar.
3. **DoD-Makefile und Staging:** Targets, FHS-Verzeichnislayout, Unitvorlagen, sysusers/tmpfiles, Installationsmanifest und DESTDIR-Tests. Ergebnis: vollständig inspizierbares Installationspaket, noch ohne Aktivierung.
4. **BPF-Build und ABI:** Toolchain pinnen, Quellen und Wirevertrag bauen, Parser/Fixtures migrieren, ELF-Struktur automatisch prüfen. Ergebnis: ladbare, versionierte Objekte statt Platzhalter.
5. **Scope und Loader:** Kernelprofile, Attach-Auswahl, korrekte Task-/cgroup-Zuordnung, Zeitkonvertierung, Ressourcenbereinigung und Verlustmetriken. Ergebnis: begrenzte echte Kernelereignisse.
6. **Sentinel-Integration:** Authentifiziertes IPC, Profilabgleich, CIDR-Regeln, Readiness, getrennte FHS-Datenpfade, Wiederanlauf. Ergebnis: Ereignis bis persistiertem Befund nachvollziehbar.
7. **Lokale Abnahme und Installation:** Staging prüfen, Systeminstallation mit erforderlicher Freigabe ausführen, Profil explizit wählen, Beobachtung aktivieren, Smoke-Test und Neustart/Rollback prüfen. Warden bleibt aus.
8. **Authority-Typkern:** Typtrennung und private Grant-Konstruktion mit Compile-Fail- und Algebra-Tests erstellen; Bootstrap-Vertrauensquellen explizit implementieren.
9. **Konsumentenmigration und Makros:** Sämtliche produktiven Konstruktionen, Snapshots, Fixtures und generierten Guards umstellen; alte API entfernen.
10. **Gesamtabnahme:** Produkt-Workspace und DoD prüfen; installierte DoD-Version erneut gegen dieselben Live-Szenarien testen; effektive Garantie und Einschränkungen dokumentieren.

## 8. Test- und Abnahmematrix

### Ohne erhöhte Rechte

- TOML: mehrere cgroups, host, unbekanntes Profil, fehlende Auswahl, leere Listen, Traversierung, ungültige CIDRs, unbekannte Sensoren.
- Scope: `/a` gegen `/ab`, Kinder ein/aus, doppelte Pfade, fehlende und wiederangelegte cgroups.
- ABI: beide Ereignistypen, IPv4/IPv6, Port >255, falsche Version/Länge, abgeschnittene Strings, keine argv-/Paketdaten.
- Loader: falsches ELF, falsche Map, mehrere unerwartete Programme, fehlende Rechte, Teilfehler und vollständiges Detach über Testdoubles.
- IPC: fremde UID, gefälschte Sensor-ID, übergroße Nachricht, Queueüberlauf, Verbindungsabbruch und Wiederanlauf.
- Installation: DESTDIR enthält alle Programme/Objekte/Units; keine Host-Seiteneffekte; bestehende Konfiguration bleibt bytegleich; keine Benutzer-Home-Pfade in Units.
- Authority: direkte Konstruktion und Deserialisierung eines Grants kompilieren nicht; Requirements verleihen keine Rechte; Snapshot kann nicht als Grant verwendet werden; jede Ableitung bleibt Teilmenge.
- Macros: gültige Deklarationen, unbekannte Permissions, fehlender Authority-Kontext und identisches Guard-Verhalten zu handgeschriebenem Code.

### Explizite Live-Abnahme

- Zwei Test-cgroups erzeugen Exec und TCP-Verbindungen zu einem lokalen Testserver; nur ausgewählte Gruppen erscheinen. Hostprofil erfasst beide erst nach ausdrücklicher Umstellung.
- IPv4 und IPv6 prüfen; fehlendes IPv6 als explizit nicht bestanden/Hostvoraussetzung dokumentieren, nicht still überspringen.
- Erlaubtes CIDR erzeugt keinen Egress-Verstoß; nicht erlaubtes Ziel erzeugt einen Befund. Keine Verbindung wird blockiert.
- PID/UID/cgroup und Zeitstempel gegen den Testprozess vergleichen; keine Zuordnung zum Probeprozess oder einem zufälligen Kernelworker.
- Rechteentzug, fehlendes Objekt, ungültige Config und Sentinel-Ausfall liefern eindeutigen Fehler-/Degradationsstatus.
- Stop/Restart hinterlässt keine BPF-Links; Neustart erzeugt keine doppelten Ereignisse durch doppelte Attachments.
- Warden und Socket bleiben inaktiv; Freeze-/Kill-Dateien werden nicht geschrieben und Firewallregeln nicht verändert.
- Beobachtung mindestens zehn Minuten betreiben; Heartbeats, Logrotation, Queue-/Ringverluste und Speicherverbrauch prüfen. Stille Fehler gelten nicht als erfolgreicher Betrieb.

Kanonische Verifikation nach Einführung der Targets: `make -C dod verify`, anschließend für Lieferung B das bestehende Root-Target `make clippy-tests`. Live-Abnahme separat mit `make -C dod smoke`. Baselinefehler müssen separat ausgewiesen werden; keine grüne Gesamtabnahme bei fehlenden Pflichtprüfungen.

## 9. Fertigkriterien und bewusst verbleibende Grenzen

Lieferung A ist fertig, wenn ein frischer Build installierbare Host- und BPF-Artefakte erzeugt, `make install` das vollständige FHS-Paket einschließlich Services bereitstellt und reale, profilgerecht begrenzte Ereignisse im Sentinel nachweisbar ankommen. Alle Abnahmeschritte müssen Belege statt ausschließlich Modulkommentare liefern.

Lieferung B ist fertig, wenn Anforderungen und Grants getrennt sind, Root-Ausstellung tatsächlich vertrauenswürdige Policy prüft, kein öffentlicher Roh-/Serde-/Makropfad beliebige Grants erzeugt und sämtliche Haupt- sowie DoD-Konsumenten mit den neuen Typen bauen und getestet sind.

Nicht Bestandteil dieser Lieferung: automatische cgroup-Eskalation, Netzwerkblockierung, UDP-Vollabdeckung, Paketmitschnitt, uneingeschränkte Isolation bösartiger nativer Plugins, fanotify-/Audit-Netlink-Ausbau. Ihre Abwesenheit wird in Status und Betriebsdokumentation sichtbar ausgewiesen.

Der Plan nutzt die bereits gelesenen Leitlinien `superpowers:brainstorming` für die geklärten Architekturentscheidungen und `superpowers:writing-plans` für Arbeitspakete, Schnittstellen und Abnahmen. Dieses Dokument ist die angeforderte ausführliche Plan-Datei; es erteilt keine zusätzliche Freigabe für privilegierte Installation oder Host-Beobachtung.
