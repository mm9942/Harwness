# `harw-dod`: Crate-Zerlegung, Zuständigkeiten und Rechtematrix

**Status:** Entwurf zur Diskussion, noch nicht normativ
**Zweck:** Die Zerlegung des Verteidigungssubsystems in kleine, einzeln
zuständige Crates; ihre Abhängigkeiten, Fehlertypen, Verknüpfungen und die
Festlegung, wer was wie wo darf
**Ersetzt:** die grobe Crate-Tabelle aus
`harw-security-observability-plan.md` §2.1 (Mapping in §11)
**Verwandt:** `harw-dod-charter.md`,
`harw-dod-integration-and-dependencies.md`, `harw-context-plan.md`

---

## 0. Das Prinzip, aus dem alles folgt

Eine Crate, eine Quelle, eine Berechtigung.

Das ist keine Ordnungsliebe, das ist der Kern der Sicherheitsarchitektur.
Wenn jede Crate genau eine Berechtigung braucht, dann ist die
Berechtigungsmenge eines Binaries die Vereinigung über seinen
Abhängigkeitsgraphen. Und diese Vereinigung ist berechenbar:
`harw-code-graph` liest ohnehin `Cargo.toml` und `Cargo.lock`.

**Damit wird das Privilegienbudget zu einer Grapheigenschaft, die in CI
prüfbar ist.** Ein Binary, das eine Berechtigung erwirbt, die es nicht haben
soll, ist kein Reviewversäumnis, sondern ein fehlgeschlagener Build. Das ist
dasselbe Muster wie überall im Haus: die Eigenschaft wird nicht überwacht,
sie ist nicht ausdrückbar.

Die Umkehrung gilt genauso und ist der eigentliche Grund für die Feinheit:
eine Crate, die Prozesse beobachtet, braucht `CAP_BPF`; eine, die
Dateiereignisse beobachtet, braucht `CAP_SYS_ADMIN`; eine, die Temperaturen
liest, braucht gar nichts. Lägen sie zusammen, hätte die Temperaturmessung
faktisch Root. Getrennt hat sie ein Leserecht auf `/sys`.

---

## 1. Invarianten der Zerlegung

Nummernkreis C, als Ergänzung zu den S- und K-Invarianten.

**C1.** Eine Crate hat genau eine Quelle und deklariert genau eine
Berechtigung. Braucht etwas zwei, sind es zwei Crates.

**C2.** Die Berechtigung wird beim Binden nachgewiesen, nicht angenommen.
`SensorHandle<Unbound>::bind()` führt einen echten Probe-Zugriff aus; nur
`SensorHandle<Bound>` hat `poll` und `subscribe`.

**C3.** Der Geltungsbereich ist ein Wert, keine Konvention. Ein Sensor liest
ausschließlich durch einen `ReadScope`, den er im Konstruktor bekommt und der
nur schrumpfen kann.

**C4.** Privilegierte Prozesse sind Push-Only. Ein Probe-Prozess mit
`CAP_BPF` oder `CAP_SYS_ADMIN` hat keine Kommandoschnittstelle. Er schiebt
Ereignisse zum Sentinel und nimmt nichts entgegen. Ein kompromittierter
Sentinel kann einen Probe nicht beauftragen, weil es nichts zu beauftragen
gibt.

**C5.** Der öffentliche Fehlertyp einer Crate ist schmal und inhaltsfrei. Ein
Parse-Fehler nennt Position und Länge, niemals den geparsten Inhalt. Sonst
wandert eine Angreifer-kontrollierte Logzeile über den Fehlerpfad in die
Telemetrie.

**C6.** Ein dauerhaft fehlschlagender Sensor degradiert das System, er stoppt
es nie. Ausfall ist ein Betriebszustand mit Metrik, kein Abbruch.

**C7.** Keine Sensor-Crate hängt von einer anderen Sensor-Crate ab. Sensoren
sind Geschwister, niemals eine Kette. Sonst blutet die Berechtigung der einen
in den Graphen der anderen.

**C8.** Das Privilegienbudget eines Binaries ist die Vereinigung der
Berechtigungen über seinen Abhängigkeitsgraphen und wird in CI gegen eine
deklarierte Obergrenze geprüft.

---

## 2. Namensschema

Alle Crates des Subsystems tragen das Präfix `harw-dod-`. Das ist keine
Kosmetik: bei 46 vorhandenen Membern und rund zwei Dutzend neuen macht das
Präfix im Abhängigkeitsgraphen sofort sichtbar, was zum Verteidigungsteil
gehört und was nicht. Die Fassade selbst heißt `harw-dod` und ist damit auch
alphabetisch der Kopf ihrer Gruppe.

Ausgenommen bleiben `harw-context` und `harw-observe`. Sie sind
Infrastruktur für alle Agenten und stehen neben dem Subsystem, nicht darin
(Charta §4).

---

## 3. Fundament-Crates

Drei Vokabular-Leaves und drei Zugriffsprimitiven. Keine davon liest selbst
eine Quelle.

| Crate | L | Deps (intern) | Verantwortung |
|---|---|---|---|
| `harw-dod-cap` | 0 | `harw-types`, `harw-macros` | `Capability`, `ReadScope`, `SensorError`, `Permanence`, der `Sensor`-Trait mit Typestate |
| `harw-dod-signals` | 1 | `harw-dod-cap`, `harw-observe`, `harw-research` | `SecurityEvent`, `EventKind`, `HostSample`, `Finding<S>`, `Verdict` |
| `harw-dod-warden-proto` | 1 | `harw-dod-signals` | Wire-Typen der geschlossenen Aktionsmenge |
| `harw-dod-readfs` | 1 | `harw-dod-cap` | Getypte Lesezugriffe unter einem `ReadScope`; Zahl, Zeile, Glob |
| `harw-dod-netlink` | 1 | `harw-dod-cap` | Netlink-Socket und Rahmen, protokoll-parametrisiert |
| `harw-dod-bpf` | 1 | `harw-dod-cap`, `harw-observe` | aya-Laderahmen, Map-Zugriff, Verifier-Fehlerabbildung |

### 3.1 `harw-dod-cap`, das Herzstück

```rust
/// Was eine Crate an Berechtigung braucht. Geschlossene Menge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// Lesen unter /sys. Keine Kernel-Berechtigung nötig.
    SysfsRead,
    /// Lesen unter /proc. Keine Kernel-Berechtigung nötig.
    ProcfsRead,
    /// Lesen der cgroup-v2-Hierarchie.
    CgroupRead,
    /// Lesen des Reports-Verzeichnisses.
    ReportsRead,
    /// Lesen der Workspace-Manifeste.
    WorkspaceRead,
    /// journald. Gruppe systemd-journal.
    JournalRead,
    /// auditd über Netlink. Gruppe adm oder CAP_AUDIT_READ.
    AuditNetlink,
    /// eBPF laden und Maps lesen. CAP_BPF plus CAP_PERFMON.
    BpfLoad,
    /// fanotify markieren. CAP_SYS_ADMIN.
    FanotifyMark,
    /// Netfilter und Namespaces schreiben. CAP_NET_ADMIN. Nur im Warden.
    NetAdmin,
    /// cgroup-Freezer schreiben. Nur im Warden.
    CgroupWrite,
}

impl Capability {
    /// Die Privilegienklasse. Bestimmt, in welches Binary die Crate darf.
    pub const fn class(self) -> PrivilegeClass;
    /// Ein realer Probe-Zugriff, der beweist, dass die Berechtigung da ist.
    pub fn probe(self, scope: &ReadScope) -> Result<(), SensorError>;
}

pub enum PrivilegeClass { Unprivileged, GroupRead, KernelObserve, KernelAct }
```

```rust
/// Das "wo". Kann nur schrumpfen. Vierte Anwendung des Halbverbands.
pub struct ReadScope { roots: BTreeSet<PathBuf> }

impl ReadScope {
    pub fn of(roots: impl IntoIterator<Item = PathBuf>) -> Self;
    pub fn intersection(&self, other: &Self) -> Self;
    /// Öffnet nur innerhalb des Scopes; löst Symlinks auf und prüft danach.
    pub fn open(&self, path: &Path) -> Result<OwnedFd, SensorError>;
    /// Für die Landlock-Regel des Prozesses.
    pub fn to_landlock_paths(&self) -> Vec<PathBuf>;
}
```

Kein `add`, kein `extend`. Ein Sensor bekommt seinen Scope im Konstruktor und
kann ihn nicht erweitern. Symlink-Auflösung vor der Prüfung ist Pflicht,
sonst ist der Scope über einen Link umgehbar.

**Der Sensor-Trait mit Typestate:**

```rust
pub struct Unbound;
pub struct Bound;

pub trait Sensor: Send + Sync {
    fn id(&self) -> SensorId;
    fn capability(&self) -> Capability;
    fn cadence(&self) -> Cadence;
}

pub struct SensorHandle<S, T: Sensor> { inner: T, scope: ReadScope, _s: PhantomData<S> }

impl<T: Sensor> SensorHandle<Unbound, T> {
    /// Führt `capability().probe(&scope)` aus. Nur bei Erfolg gebunden.
    pub fn bind(self) -> Result<SensorHandle<Bound, T>, SensorError>;
}

impl<T: Sensor> SensorHandle<Bound, T> {
    pub fn poll(&self, out: &mut SampleSink) -> Result<(), SensorError>;
    pub fn subscribe(&self, out: EventTx) -> Result<Subscription, SensorError>;
}
```

**Der Grenz-Fehlertyp:**

```rust
#[derive(Debug, HarwError)]
pub enum SensorError {
    #[msg("capability {0:?} not available")]
    CapabilityDenied(Capability),
    #[msg("path outside read scope")]
    OutsideScope,
    #[msg("source unavailable: {0}")]
    SourceUnavailable(SensorId),
    #[msg("parse failed at byte {offset} of {len}")]
    Parse { offset: usize, len: usize },
    #[msg("source degraded: {0}")]
    Degraded(SensorId),
}

impl SensorError {
    /// Entscheidet über Wiederholen oder Abmelden (C6).
    pub fn permanence(&self) -> Permanence;
}

pub enum Permanence {
    /// Nächster Poll versuchen.
    Transient,
    /// Sensor abmelden, Metrik hochzählen, System läuft weiter.
    Permanent,
}
```

`Parse` trägt Offset und Länge, nie Inhalt (C5). Das ist die Stelle, an der
sonst eine Angreifer-kontrollierte Logzeile in die Telemetrie wandert.

---

## 4. Sensor-Crates, Strom A: Messwerte

Alle unprivilegiert, alle nur lesend, alle ohne Abhängigkeit voneinander
(C7). Jede hängt an `harw-dod-cap`, `harw-dod-signals`, `harw-dod-readfs`
und sonst nichts.

| Crate | Quelle | Capability | Liefert | Darf nicht |
|---|---|---|---|---|
| `harw-dod-thermal` | `/sys/class/hwmon`, `/sys/class/thermal` | SysfsRead | Temperatur, Lüfter, Spannung, Throttle-Flags | Prozesse auflösen, `/proc` lesen |
| `harw-dod-cpu` | `/proc/stat`, cpufreq, `/proc/pressure/cpu` | ProcfsRead | Zeitklassen je Kern inkl. `steal`, Frequenz, PSI | einzelne PIDs lesen |
| `harw-dod-memory` | `/proc/meminfo`, `/proc/pressure/memory`, `/proc/vmstat` | ProcfsRead | Belegung, Swap-Raten, PSI | Prozessspeicher inspizieren |
| `harw-dod-blockio` | `/proc/diskstats`, `/proc/pressure/io`, `statvfs` | ProcfsRead | Latenz, Queue-Tiefe, Füllstände, PSI | Dateiinhalte lesen |
| `harw-dod-netcounters` | `/proc/net/dev`, conntrack-Zähler | ProcfsRead | Zähler je Interface | Sockets, Verbindungen oder Ziele auflösen |
| `harw-dod-gpu` | `/sys/class/drm`, hwmon; NVML hinter Feature | SysfsRead | Auslastung, VRAM, Temperatur, Throttle-Gründe | Prozesse je GPU zuordnen ohne Cgroup-Bezug |
| `harw-dod-cgroup` | `cpu.stat`, `memory.current`, `io.stat`, `pids.current` | CgroupRead | Ressourcen je cgroup, hierarchisch | in die Hierarchie schreiben |

**Zur Abgrenzung `netcounters` gegen `listener` gegen `flow`:** drei Crates,
drei Berechtigungen, drei Fragen. Zähler je Interface sind harmlos und
unprivilegiert. Offene Sockets erfordern die Inode-Auflösung nach PID und
sind schon deutlich sensibler. Verbindungen mit Zielen und SNI brauchen eBPF
und einen privilegierten Prozess. Die Zusammenlegung wäre bequem und würde
die harmloseste Messung auf das höchste Privileg heben.

---

## 5. Sensor-Crates, Strom B: Ereignisse

| Crate | Quelle | Capability | Prozess | Liefert | Darf nicht |
|---|---|---|---|---|---|
| `harw-dod-listener` | `/proc/net/tcp{,6}`, `/proc/net/udp`, Inode→PID | ProcfsRead | sentinel | offene Listener mit cgroup-Bezug | Verbindungsinhalte, Ziele |
| `harw-dod-authlog` | auditd `USER_AUTH`/`USER_LOGIN`, journald sshd-Unit, btmp | AuditNetlink, JournalRead | sentinel | Login-Versuche und Erfolge mit `auid`, Quelle, Methode | Netzwerkflows, Dateiereignisse |
| `harw-dod-scanreport` | Reports von rkhunter, ClamAV, Lynis, AIDE, smartctl | ReportsRead | sentinel | normalisierte Scan-Befunde | einen Scanner starten |
| `harw-dod-workspace` | `Cargo.toml`, `Cargo.lock` über `harw-code-graph` | WorkspaceRead | sentinel | Struktur- und Dependency-Drift | Subprozesse, Netz |
| `harw-dod-procmon` | eBPF `sched_process_exec`/`exit`, procfs-Abgleich | BpfLoad | probe-bpf | Exec, Exit, Elternschaft, cgroup | Dateiereignisse, Netzwerk |
| `harw-dod-flow` | eBPF/XDP Flow-Aggregation, SNI, DNS-Namen | BpfLoad | probe-bpf | Fünftupel, Bytes, Dauer, SNI, cgroup | Nutzlast über den SNI hinaus |
| `harw-dod-fsmon` | fanotify `FAN_MODIFY`/`FAN_CLOSE_WRITE`, loginuid | FanotifyMark | probe-fs | Pfad, Zeitpunkt, Actor, Digest davor/danach | Netzwerk, Prozessereignisse |

**`harw-dod-authlog` braucht zwei Berechtigungen und verletzt damit
scheinbar C1.** Auflösung: es sind zwei Backends derselben Quelle
(Login-Ereignisse), und der Trait wird zweimal implementiert,
`AuditBackend` und `JournalBackend`, jedes mit genau einer Berechtigung. Der
Sentinel bindet, was verfügbar ist, und degradiert auf das andere. C1 gilt
pro Backend, nicht pro Crate; das ist die einzige Ausnahme und sie ist hier
notiert.

**`harw-dod-scanreport` startet nichts.** Ein systemd-Timer fährt die
Werkzeuge mit festen Argumenten und schreibt in das Reports-Verzeichnis. Die
Crate liest ausschließlich. Es gibt in ihrem Code keinen Pfad, der eine
Kommandozeile bildet.

---

## 6. Verarbeitung und Durchsetzung

| Crate | L | Deps (intern) | Verantwortung | Prozess |
|---|---|---|---|---|
| `harw-dod-sentinel` | 3 | alle unprivilegierten Sensoren, `harw-observe` | Aggregation, Poll-Schleifen, Degradation, Ringpuffer, IPC-Empfang von den Probes | sentinel |
| `harw-dod-rules` | 3 | `harw-dod-signals`, `harw-plan`, `harw-sandbox`, `harw-knowledge` | Vertrags-, Schwellen- und Abweichungsregeln; reine Funktionen mit injiziertem `now` | sentinel |
| `harw-dod-escalate` | 4 | `harw-dod-rules`, `harw-dod-warden-proto`, `harw-knowledge`, `harw-plan-bridge` | Leiter, alleiniger Konstruktor von `Action<Authorized>`, Freeze-Leases, Plan-Andockung | sentinel |
| `harw-dod-netpolicy` | 2 | `harw-sandbox`, `harw-dod-cap` | `NetPlan` als reiner, testbarer Plan | warden |
| `harw-dod-warden` | 2 | `harw-dod-warden-proto`, `harw-dod-netpolicy`, `harw-observe` | Durchsetzung, geschlossene Aktionsmenge, Audit | warden |
| `harw-dod` | 5 | Fassade | Reexport, Prelude, keine Logik | — |

`harw-dod-sentinel` enthält **keine** Parselogik. Es hält Sensoren, ruft sie,
verteilt in die Ströme und verwaltet Degradation. Wer Parsen sucht, findet es
in der Quell-Crate.

---

## 7. Prozess-Topologie

Vier Prozesse, je eine Privilegienklasse. Das ist die Umsetzung von C4 und
C8 auf Betriebsebene.

| Binary | Nutzer | Kernel-Rechte | Enthält | Landlock-Scope |
|---|---|---|---|---|
| `harw-sentinel` | eigener Nutzer, Gruppen `adm`, `systemd-journal` | keine | Strom-A-Sensoren, listener, authlog, scanreport, workspace, rules, escalate | ro: `/proc`, `/sys`, Reports-Dir, Workspace-Manifeste |
| `harw-probe-bpf` | eigener Nutzer | `CAP_BPF`, `CAP_PERFMON` | procmon, flow | ro: `/sys/fs/bpf`, `/proc` |
| `harw-probe-fs` | eigener Nutzer | `CAP_SYS_ADMIN` | fsmon | ro: die überwachten Pfade plus `/proc` |
| `harw-warden` | eigener Nutzer | `CAP_NET_ADMIN`, cgroup-Schreibrecht | netpolicy, warden | rw: nur `cgroup.freeze`-Dateien |

**Warum `harw-probe-fs` allein steht.** fanotify mit
Filesystem-Markierung verlangt `CAP_SYS_ADMIN`, und das ist praktisch Root.
Ein Prozess mit dieser Berechtigung darf neben sich nichts anderes haben.
Sein Code ist entsprechend klein: eine Crate, ein Socket, ein Push-Loop.

**Datenflussrichtung.** Probes schieben, Sentinel empfängt, Sentinel schlägt
vor, Warden führt aus.

```
probe-bpf ──push──┐
probe-fs  ──push──┼──> sentinel ──WardenRequest──> warden
                  │       │
sentinel-eigene ──┘       └──> Artefakte, Telemetrie, Plan-Bridge
```

Kein Pfeil zeigt vom Sentinel zu einem Probe. Die Probes haben keinen
Empfangspfad, ihr Socket ist schreibseitig. Ein kompromittierter Sentinel
gewinnt dadurch nichts vom `CAP_SYS_ADMIN` des Probes.

**Der Warden empfängt** und ist der einzige, der das tut. Sein Socket ist
systemd-aktiviert, mit `SO_PEERCRED`-Prüfung, versioniertem Rahmen und der
Aktionsmenge als einzigem Nachrichtenvokabular.

---

## 8. Rechtematrix: wer darf was wie wo

Vier Dimensionen, jede mit einem eigenen Durchsetzungsmechanismus. Das ist
die Antwort auf die Kernfrage dieses Dokuments.

| Dimension | Frage | Träger | Durchgesetzt durch |
|---|---|---|---|
| **Wer** | welche Crate, welcher Prozess | Crate-Identität, systemd-Unit | Abhängigkeitsgraph plus CI-Gate (C8) |
| **Was** | welche Berechtigung | `Capability` | Probe beim `bind()` (C2), Kernel-Capability der Unit |
| **Wo** | welche Pfade, welches Netlink-Protokoll, welche cgroup | `ReadScope`, Protokoll-Parameter | Typ plus Landlock-Regel des Prozesses (C3) |
| **Wie** | lesen, abonnieren, handeln | Typestate | `SensorHandle<Bound>` für Lesen und Abonnieren, `Action<Authorized>` für Handeln |

**Die vier Mechanismen sind unabhängig und alle deny-only.** Ein Sensor, der
seine Berechtigung nicht nachweisen kann, ist nicht aufrufbar. Einer, dessen
Pfad außerhalb des Scopes liegt, bekommt kein Filedeskriptor. Ein Prozess,
dem Landlock den Pfad verwehrt, sieht ihn nicht, egal was sein Code tut. Und
eine Aktion ohne Autorisierung existiert typseitig nicht.

**Konkret pro Rolle:**

| Rolle | darf lesen | darf abonnieren | darf handeln |
|---|---|---|---|
| Strom-A-Sensor | seine eine Quelle im Scope | nein | nein |
| Ereignis-Sensor unprivilegiert | seine eine Quelle im Scope | ja, seine Quelle | nein |
| Ereignis-Sensor privilegiert | seine Kernelquelle | ja, seine Quelle | nein, kein Empfangspfad |
| Sentinel | nichts direkt, nur über Sensoren | ja, alle gebundenen | nein, nur vorschlagen |
| Regelschicht | übergebene Referenzen | nein | nein |
| Triage-Agent | Kontext gemäß `ContextCeiling` | nein | nein, nur `ProposedAction` |
| Leiter | Befunde, Baselines | nein | `Action<Authorized>` konstruieren |
| Warden | seine eigene Konfiguration | nein | die geschlossene Aktionsmenge |
| Mensch | alles über Operator-Flächen | ja | auch das Irreversible |

Die Zeile, die das System trägt: **kein Agent und kein Modell steht in der
Spalte "darf handeln".** Der Weg dorthin führt ausschließlich über die
Leiter, und die entscheidet nach Härte des Befunds, nicht nach Urteil.

---

## 9. Fehlerbehandlung und Verknüpfung

**Zwei Ebenen pro Crate.** Innen ein reicher, spezifischer Fehlertyp
(`ThermalError`, `AuthlogError`, …) mit allem, was beim Debuggen hilft.
Außen, an der Trait-Grenze, die Abbildung auf `SensorError`. Der reiche Typ
ist `pub`, damit man ihn testen kann, erscheint aber in keiner
Trait-Signatur.

```rust
// in harw-dod-authlog
#[derive(Debug, HarwError)]
pub enum AuthlogError {
    #[msg("netlink socket error: {0}")] Socket(NetlinkError),
    #[msg("audit record type {0} not handled")] UnknownRecord(u16),
    #[msg("record truncated at field {index}")] Truncated { index: usize },
    #[msg("journal cursor lost")] CursorLost,
}

impl From<AuthlogError> for SensorError {
    fn from(e: AuthlogError) -> Self { /* verdichtet, inhaltsfrei */ }
}
```

**Verdichtung ist Pflicht, nicht Bequemlichkeit.** Die Abbildung wirft
Kontext weg, und genau das ist gewollt: `AuthlogError::Truncated { index }`
wird zu `SensorError::Parse { offset, len }`, und kein Feldinhalt überlebt
den Übergang (C5).

**Degradation als Zustand.** Der Sentinel führt pro Sensor einen kleinen
Automaten:

```rust
pub enum SensorState {
    Bound,
    Retrying { since: Timestamp, attempts: u32 },
    Degraded { since: Timestamp, reason: SensorError },
}
```

`Permanence::Transient` führt nach `Retrying` mit Backoff,
`Permanence::Permanent` direkt nach `Degraded`. Ein degradierter Sensor wird
abgemeldet, `sensor_degraded_total` zählt hoch, und die Regelschicht erfährt
über einen `EventKind::SensorDegraded`, dass ihr eine Quelle fehlt. Das ist
wichtig: eine Abweichungsregel, deren Sensor stumm ist, darf nicht
stillschweigend "keine Auffälligkeit" bedeuten. Blindheit ist ein Befund.

**Fehler im Warden.** Eigener, noch schmalerer Typ. Jeder Fehlerpfad
schreibt einen Audit-Eintrag, bevor er zurückkehrt, und der Nullzähler
`warden_unaudited_actions` prüft genau das.

---

## 10. Bauordnung

Bottom-up, jede Stufe einzeln nützlich und testbar. Die Stufen 2 bis 4 sind
crate-disjunkt und damit als Zelle mit `write_partition = Required` parallel
fahrbar.

**Stufe 1, Fundament.** `harw-dod-cap` mit `Capability`, `ReadScope`,
`SensorError`, Typestate. Dazu `harw-dod-signals`. Beides Leaves, beides
ohne Seiteneffekt, beides vollständig unit-testbar.

**Stufe 2, Primitiven.** `harw-dod-readfs` (mit Symlink-Auflösung und
Scope-Test), `harw-dod-netlink`, `harw-dod-bpf`. Jede gegen ein Fixture-Dir
beziehungsweise gegen einen Loopback-Socket testbar, ohne Privilegien.

**Stufe 3, die unprivilegierten Sensoren, parallel.** Sieben Strom-A-Crates
plus `listener`, `authlog` (Journal-Backend), `scanreport`, `workspace`.
Jede mit einem Fixture-Verzeichnis, das echte `/proc`- und `/sys`-Auszüge
enthält, sodass die Parser ohne die Maschine getestet werden.

**Stufe 4, Sentinel.** Aggregation, Degradationsautomat, Ringpuffer, Metriken.
Erste lauffähige Binary, komplett unprivilegiert, sofort nützlich.

**Stufe 5, Regeln.** `harw-dod-rules` mit Vertragsregeln über
`validate_patch`, Schwellen aus Baselines, Abweichungen. Reine Funktionen,
goldene Fälle.

**Stufe 6, privilegierte Probes.** `harw-dod-fsmon` mit eigenem Binary,
dann `harw-dod-procmon` und `harw-dod-flow`. Ab hier braucht es eine VM zum
Testen; deshalb so spät wie möglich.

**Stufe 7, Leiter und Warden.** `harw-dod-escalate`, `harw-dod-warden-proto`,
`harw-dod-netpolicy`, `harw-dod-warden`. Erst hier kann irgendetwas gehandelt
werden.

**Stufe 8, Fassade und Agenten.** `harw-dod`, Security-Familie, Clan,
Triage-Spezialisierungen, Plan-Andockung.

Bemerkenswert an dieser Ordnung: bis einschließlich Stufe 5 braucht keine
Zeile Code eine Kernel-Berechtigung. Fünf von acht Stufen sind auf deinem
Arbeitsrechner ohne VM entwickelbar und testbar.

---

## 11. Mapping zur alten Crate-Tabelle

| Alt (Security-Plan §2.1) | Neu |
|---|---|
| `harw-signals` | `harw-dod-signals` |
| `harw-warden-proto` | `harw-dod-warden-proto` |
| `harw-sensor` (eine Crate) | `harw-dod-cap` plus drei Primitiven plus vierzehn Sensor-Crates |
| `harw-netpolicy` | `harw-dod-netpolicy` |
| `harw-warden` | `harw-dod-warden` |
| `harw-rules` | `harw-dod-rules` |
| `harw-escalate` | `harw-dod-escalate` |
| `harw-observe` | unverändert, bleibt Infrastruktur ohne Präfix |
| — | neu: `harw-dod-sentinel`, `harw-dod`, `harw-dod-readfs`, `harw-dod-netlink`, `harw-dod-bpf` |

Aus acht Crates werden vierundzwanzig plus die Fassade. Der Zuwachs sitzt
fast vollständig in Stufe 3, also in kleinen, gleichförmigen,
unprivilegierten Parsern, und genau das ist beabsichtigt: das ist die
Schicht, in der Feinheit billig ist und Zusammenlegung teuer wäre.

---

## 12. Prüfungen

**Privilegien-Gate.** Ein CI-Schritt berechnet je Binary die Vereinigung der
`Capability`-Deklarationen über den Abhängigkeitsgraphen und vergleicht mit
der deklarierten Obergrenze der Unit. Abweichung bricht den Build (C8).

**Isolations-Test.** Ein Test prüft, dass keine Sensor-Crate eine andere
Sensor-Crate in ihren Abhängigkeiten hat (C7). Zwei Zeilen über
`harw-code-graph`, und damit prüft das System diese Invariante mit seinem
eigenen Werkzeug.

**Scope-Test.** Ein Fixture-Verzeichnis mit einem Symlink, der aus dem Scope
hinauszeigt; `ReadScope::open` muss `OutsideScope` liefern. Der klassische
Fehler wäre, vor dem Auflösen zu prüfen.

**Fehler-Redaktionstest.** Für jede Sensor-Crate ein Fall mit einer
Angreifer-Zeile, deren Inhalt in keiner Ausgabe von `SensorError` erscheinen
darf. Property-Test über die `From`-Implementierungen.

**Degradations-Test.** Ein Sensor, der dauerhaft `Permanent` liefert, muss
den Sentinel in `Degraded` bringen, ohne ihn zu beenden, und ein
`SensorDegraded`-Ereignis erzeugen.

**Push-Only-Test.** Der Probe-Socket wird beschrieben und darf beim Lesen
nichts liefern; ein Test prüft, dass das Probe-Binary keine
Empfangsschleife enthält (C4).

**Fixture-Korpora.** Je Sensor ein Verzeichnis mit echten Auszügen
verschiedener Kernelversionen. Das ist die einzige Möglichkeit, Parser gegen
Formatdrift zu sichern, ohne für jede Version eine VM zu fahren.

---

## 13. Offene Entscheidungen

1. **IPC-Format Probe zu Sentinel.** Vorschlag: längenpräfixierte Rahmen mit
   einem kompakten Binärformat statt JSON, weil die Ereignisrate bei
   `procmon` hoch sein kann. Alternative wäre ein eBPF-Ringpuffer, den der
   Sentinel selbst liest; das würde ihm aber `CAP_BPF` geben und die
   Trennung aufheben. Empfehlung: Socket, nicht geteilter Ringpuffer.
2. **Obergrenze der Sensor-Crates.** Vierzehn ist der aktuelle Schnitt.
   Wenn eine fünfzehnte Quelle dazukommt, ist die Frage, ob sie eine eigene
   Crate ist oder ein zweites Backend einer bestehenden. Regel: eigene
   Quelle plus eigene Berechtigung gleich eigene Crate.
3. **`harw-dod-cgroup` gegen `harw-dod-warden`.** Lesen und Schreiben der
   cgroup-Hierarchie sind getrennte Berechtigungen und getrennte Crates.
   Offen ist, ob der Warden für das Einfrieren die Lese-Crate mitbenutzt
   oder eine eigene minimale Leseroutine hat. Tendenz: eigene, weil jede
   Abhängigkeit im Warden gegen D7 zählt.
4. **Landlock-Mindestversion.** Die ABI-Abfrage ist Pflicht; offen ist, ob
   ein Kernel ohne Landlock ein harter Startfehler oder eine dokumentierte
   Degradation ist. Tendenz: Degradation mit lauter Metrik, weil sonst
   ältere Zielsysteme ausgeschlossen wären.
5. **Fixture-Herkunft.** Auszüge aus welchen Kernelversionen, und wie werden
   sie gepflegt. Vorschlag: die Versionen, die Fedora und CentOS Stream
   aktuell ausliefern, plus eine ältere LTS-Linie.
