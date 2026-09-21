# Harwness DoD × Context: Integrationsplan und Abhängigkeits-Doktrin

**Status:** Entwurf zur Diskussion, noch nicht normativ
**Basis:** 46 Member, 205 Kanten, Tiefe 8
**Bündelt:** `harw-security-observability-plan.md` (S-Invarianten, acht Crates)
und `harw-context-plan.md` (K-Invarianten, `harw-context`)
**Ergänzt:** `harw-dod-charter.md` (Name, Fassade, Absicht)
**Recherchestand:** August 2026

---

## 0. Warum diese beiden Pläne einen gemeinsamen haben müssen

Sie sind nicht benachbart, sie greifen ineinander. Vier Berührungen sind so
eng, dass getrennte Umsetzung doppelte Mechanik erzeugen würde:

1. **S6 (Netz und Sicherheitskontext disjunkt) ist ohne die
   `ContextCeiling` nicht durchsetzbar.** Eine Ceiling über Permissions sagt,
   was ein Agent *tun* darf. S6 verlangt eine Aussage darüber, was er *sehen*
   darf. Genau das ist K2.
2. **Die Injection-Grenze des DoD-Plans (§7.4) und die Vertrauensklassen des
   Kontextplans (K4) sind derselbe Mechanismus.** Zweimal gebaut wären es zwei
   Renderpfade und zwei Fixture-Sätze.
3. **`harw-observe` gehört beiden.** Es entstand im DoD-Plan, die
   Kontextmetriken leben darin, und die Kalibrierschleife (K7) speist den
   Layer-4-Rückkanal des DoD-Plans.
4. **Sicherheitsbefunde sind der schwerste Kontextfall, den es gibt.** Große
   Evidenz, hohe Sensibilität, lokale Inferenz. `DetailMode::References` ist
   für sie gebaut, nicht nebenbei.

Deshalb: eine Wellenfolge, ein Vokabularsatz, ein Fixture-Korpus.

---

## 1. Die vier Nahtstellen im Detail

### 1.1 S6 wird eine Mengenaussage über Sichtbarkeit

```toml
# harwness.ceiling.security@1
[context]
universe  = ["goal.*", "plan.*", "security.*", "knowledge.palace",
             "memory.*", "project.root", "history.tail"]
forbidden = ["net.*", "transcript.sibling.*", "secrets.*"]
budget_max = { tokens_total = 8000 }

# harwness.ceiling.intel-scout@1
[context]
universe  = ["net.advisory", "project.deps", "history.tail"]
forbidden = ["security.*", "goal.*", "plan.*", "memory.*", "secrets.*"]
budget_max = { tokens_total = 4000 }
```

`ContextCeiling::intersect` ist monoton, `admits` prüft beim Auflösen. Damit
ist die Disjunktheit weder Prompt-Konvention noch Reviewregel, sondern eine
Eigenschaft, die der Resolver abweist. Die Attacker-Fixture
`context_escalation_attacker.toml` prüft genau das für beide Richtungen.

### 1.2 Eine Injection-Grenze für beide Pläne

`TrustClass` aus `harw-context` ist der Träger. Ereignisdaten des DoD-Plans
sind `Data`, digest-gesicherte Nachweise `Evidence`, Instruktionen kommen
ausschließlich aus Definitionen. Der Zwei-Block-Renderer (K4) ist der einzige
Ort, an dem entschieden wird, was in den Instruktionsteil darf.

Ein Fixture-Korpus, zwei Prüfungen: die Injection-Fixtures des DoD-Plans
laufen zusätzlich durch den Renderer und müssen im Datenblock landen, und der
Nullzähler `trust_block_violation` ist die gemeinsame Metrik.

### 1.3 Ein Digest-Typ

`ContentDigest` (blake3, wie `SnapshotId`) lebt in `harw-types` und wird
dreifach benutzt: `Fragment.digest` für Cache-Stabilität, `SecurityEvidence`
für Manipulationserkennung, `EvidenceRef.digest` (die additive Erweiterung in
`harw-plan`) für Plan-Nachweise. Ein Typ, drei Verwendungen, keine
Konvertierung.

### 1.4 Referenzmodus als Sicherheitsmechanismus

`SampleRing::freeze()` liefert eine `SecurityEvidence` mit Digest, nicht
Rohdaten. Im Kontext erscheint sie als Referenz, das Nachladen läuft über
`context.load` mit Kappe. Damit ist die hochauflösende Evidenz vorhanden,
verlässt aber nie ungefragt die Platte, und der Triage-Turn bleibt bei
wenigen tausend Token, was lokale Inferenz überhaupt erst ermöglicht.

---

## 2. Verschmolzene Wellenfolge

Ersetzt die getrennten S- und K-Wellen. Jede Welle ist als Zelle
formulierbar, Schreibbereiche crate-disjunkt, keine hängt an einer späteren.

| Welle | Thema | Enthält |
|---|---|---|
| **W0** | Vokabular | `harw-observe`, `harw-context`, `harw-signals`, IDs in `harw-types`, `metrics!`, `field!`, `Redact`, `Fragment::from_v1` |
| **W1** | Selbstbeobachtung und Assembly v2 | `TraceContext` in `StoredJob`/`ChildLeaseRecord`, `#[traced]`, Typestate-Assembly, **die IR wird erstmals konsumiert**, `must_include` fail-closed, Plan-Schleifen-Metriken |
| **W2** | Programme und Beobachter | Kontextprogramme als Definitionen, `ContextCeiling` im Resolver und im Handoff, `harw-sensor` sysfs/procfs/journald plus `WorkspaceSensor` |
| **W3** | Mengen | `EgressSet` in `harw-sandbox`, `harw-netpolicy` als reiner Plan, Quellenbindungen (Plan-, Memory-, Knowledge-Provider) |
| **W4** | Grenze und Regeln | Zwei-Block-Rendering, Vertrauensklassen, `harw-rules` mit Vertragsregeln, `FileSensor` mit loginuid, Baselines, `EvidenceRef`-Digest |
| **W5** | Durchsetzung und Referenzen | `harw-warden-proto`, `harw-warden`, `harw-escalate`, `FreezeStore`, `warden_actions!`, `DetailMode::References` mit `context.load`, Historie als Sektion |
| **W6** | Agenten | Security-Familie und Clan, vier Spezialisierungen, Programmbibliothek, Plan-Andockung, Layer-4-Schreiber |
| **W7** | Betrieb | eBPF-Backends, systemd-Units, Off-Host-Spiegel, Out-of-Band-Meldeweg, Intel-Scout, Kettenprüfung |

**Die kritische Reihenfolge:** W1 vor allem anderen, weil dort das
`ContextProgram` der `ExecutableAgentIr` zum ersten Mal durchgesetzt wird.
Solange `must_include` still wegfallen kann, ist jede Sicherheitszusage über
Kontextinhalte wertlos. Und W4 vor W6, weil ein Triage-Agent ohne
Zwei-Block-Rendering ein Injection-Ziel wäre.

---

## 3. Abhängigkeits-Doktrin

### 3.1 Die Regeln

**D1. Im privilegierten Pfad nur reines Rust.** Der Warden und alles, was er
linkt, enthält keinen C-Code, keine `*-sys`-Crate, kein `bindgen`, kein
`pkg-config`. Eine C-Bibliothek im Warden wäre genau die
Speichersicherheitslücke, gegen die das ganze System gebaut ist.

**D2. Kein `build.rs`, das einen C-Compiler startet.** Nicht im Warden-Baum,
nicht in seinen Abhängigkeiten. Build-Skripte laufen mit den Rechten des
Bauenden und sind der meistgenutzte Supply-Chain-Vektor.

**D3. Syscall-Boden ist `rustix` mit `linux_raw`.** Direkte Syscalls über
`asm!`, ohne libc, ohne `errno`, ohne pthread-Cancellation, mit erhaltener
Speicher- und I/O-Sicherheit bis zum Syscall hinunter. `libc` nur dort, wo
`rustix` nicht hinreicht, und dann isoliert in einem Modul.

**D4. Jede externe Abhängigkeit liegt hinter einem eigenen Trait.** Präzedenz
ist die Provider-Schicht: der Katalog kennt 22 Anbieter, der Code kennt einen
Trait. Ein Sensor-Backend, ein Netzwerk-Backend, ein Telemetrie-Sink sind
austauschbar, ohne dass ein Typ der Fassade sich ändert.

**D5. Pre-1.0-Abhängigkeiten erscheinen nie in einem öffentlichen Typ.** Sie
dürfen hinter einem Adapter leben, aber kein `pub fn` der Fassade gibt sie
zurück oder nimmt sie an. Sonst wird ein fremder Minor-Bruch zu einem Bruch
deiner API.

**D6. Selbst bauen, wenn die Syscall-Fläche klein und die Crate schwach ist.**
Faustregel: unter etwa 500 Zeilen Bindungscode und eine Crate mit wenig
Verkehr oder ohne aktive Pflege bedeutet: selbst bauen auf `rustix`. Eine
schwach gepflegte Abhängigkeit ist teurer als der Code, den sie ersetzt.

**D7. Die Abhängigkeitszahl des Wardens ist eine Zahl mit Obergrenze.** Sie
steht im Doc, wird in CI geprüft und ist eine Metrik. Ein Durchsetzer, den
man vollständig lesen kann, muss auch einen Abhängigkeitsbaum haben, den man
vollständig lesen kann. Vorschlag: höchstens zwölf transitive Crates.

**D8. Lieferketten-Gates in CI.** `cargo deny` für Advisories, Quellen,
Lizenzen und Bans als Basisgate; `cargo vet` zusätzlich, aber nur für den
DoD-Teilbaum, weil sein Aufwand nur bei sicherheitskritischem Code
gerechtfertigt ist; `cargo auditable` für ausgelieferte Binaries, damit der
Abhängigkeitsbaum im Binary steckt und nachträglich prüfbar ist. Jeder
`ignore`-Eintrag ist zeitlich befristet und begründet.

**D9. Die Doktrin ist selbst eine Regel.** `harw-code-graph` liest
`Cargo.lock`, der `WorkspaceSensor` erzeugt `StructureDrift`, und
`harw-rules` prüft neue Abhängigkeiten gegen das kuratierte Inventar. Eine
Abhängigkeit außerhalb der Doktrin ist damit kein Reviewversäumnis, sondern
ein Befund mit Härte `RuleTriggered`.

### 3.2 Bewertete Auswahl

**Tier A, reines Rust, empfohlen.**

| Zweck | Crate | Begründung | Risiko |
|---|---|---|---|
| Syscalls | `rustix` (linux_raw) | Bytecode Alliance, kein libc, I/O-Safety bis zum Syscall, Feature-gated, breit im Ökosystem verankert | pre-1.0, aber sehr stabil und weit verbreitet; hinter D5 halten |
| eBPF | `aya` | Von Grund auf in Rust, ohne libbpf und bcc, nur libc für Syscalls; BTF und CO-RE, kein C-Toolchain, mit musl ein einziges portables Binary | pre-1.0 (0.13.x); Verifier fängt Programmfehler ab, Ladefehler statt Absturz |
| seccomp | `seccompiler` | rust-vmm, in Firecracker im Einsatz; Filter als Rust-IR, Kompilierung zur Bauzeit über Build-Skript und `include_bytes!` | reine Rust-Codegen, kein libseccomp |
| Pfadbeschränkung | `landlock` | Unprivilegiert, nur `no_new_privs` nötig; ABI-Versionierung erlaubt Best-Effort-Kompatibilität über Kernelversionen | ABI-Abfrage ist Pflicht, sonst bricht es auf älteren Kerneln |
| Netfilter | `rustables` | Fork von `nftnl-rs`, spricht Netlink direkt, **ohne** libnftnl und libmnl | Doku nennt die API selbst rau und in Teilen durch Ausprobieren entstanden; hinter D4 kapseln, Plan-Ebene testbar halten |
| Netlink-Transport | `netlink-sys`, `netlink-packet-core`, `netlink-packet-route`, `netlink-packet-audit` | Bausteinfamilie, reine Paketzerlegung, tokio-Integration optional | viele kleine Crates; Versionsdrift im Auge behalten |
| procfs | `procfs` | reines Parsen von `/proc` | Alternative: selbst parsen, siehe unten |
| Zeit, Hash, Serde | `jiff`, `blake3`, `serde` | bereits im Baum, Konvention gesetzt | jiff pre-1.0, Naht dokumentiert |

**Tier B, C-Abhängigkeit, vermeiden oder isolieren.**

| Zweck | Kandidat | Urteil |
|---|---|---|
| Netfilter | `nftnl` / `nftnl-sys` | **Abgelehnt.** Bindgen auf libnftnl plus libmnl, pkg-config, Versionsfeatures pro libnftnl-Version. Verstößt gegen D1 und D2. |
| seccomp | libseccomp-Bindungen | **Abgelehnt** zugunsten von `seccompiler`. |
| GPU | `nvml-wrapper` | **Nur hinter Feature und Trait.** NVML ist NVIDIAs C-Bibliothek und für NVIDIA-Metriken unumgänglich; AMD und Intel laufen über sysfs ohne jede Abhängigkeit. Der Warden linkt sie nie. |

**Tier C, selbst bauen.**

| Zweck | Warum selbst | Aufwandsschätzung |
|---|---|---|
| fanotify | Verfügbare Crates sind dünn und wenig frequentiert; die Fläche ist klein: `fanotify_init`, `fanotify_mark`, Event-Read, `loginuid`-Auflösung über procfs | 300 bis 400 Zeilen auf `rustix` |
| auditd-Records | Transport über `netlink-packet-audit`, aber das Zerlegen der Recordfelder für `USER_AUTH` und `USER_LOGIN` ist die eigentliche Arbeit und soll typisiert und getestet sein | 200 bis 300 Zeilen plus Fixtures |
| sysfs, hwmon, cgroup v2, diskstats, PSI | Zeilenweises Parsen fester Formate; jede Abhängigkeit hier ist teurer als der Code | je 50 bis 150 Zeilen, gemeinsam über `#[derive(SensorSource)]` |
| Metrik-Registry und Sink | `harw-observe` ist ohnehin der Vertrag; ein eigenes Registry-Slice ist trivial | bereits im Plan |

**Tier D, bewusst außen vor.**

`opentelemetry` und `opentelemetry-otlp` sind ausdrücklich **keine**
Kernabhängigkeit. Stand Mitte 2026 auf der 0.32-Linie sind Logs und Metriken
stabil, Traces noch Beta, alle Erstanbieter-Crates weiterhin pre-1.0 mit
Brüchen in Minor-Releases und gemeinsamer Versionierung im Gleichschritt.
Genau dafür existiert `TelemetrySink`: die OTel-Bindung lebt in einem
optionalen Adapter-Crate hinter dem Trait, und der Kern kennt sie nicht.
Dasselbe gilt für Prometheus. Wenn OTel-Rust 1.0 erreicht, ist der Wechsel
ein Adapter, kein Umbau.

Ebenfalls außen vor: jede Crate, die einen Scanner-Prozess für uns startet.
Scanner laufen per systemd-Timer mit festen Argumenten, wir lesen Reports.

### 3.3 Der Warden als Härtefall

Der Warden ist der Ort, an dem die Doktrin am striktesten gilt. Sein
Zielbild: `rustix` für Syscalls, `rustables` für die Regelseite, ein
Netlink-Transport, `serde` plus ein kompaktes Format für das Protokoll,
`harw-warden-proto` und `harw-observe` aus dem eigenen Haus. Kein tokio, wenn
ein blockierender Socket-Loop reicht. Kein Logging-Framework, das dynamisch
formatiert. Kein Netzwerkzugang außer dem Unix-Socket. Kein `String`-Feld,
das je als Befehl gelesen wird.

Ein systemd-aktivierter Socket bedeutet außerdem: der Warden läuft gar nicht,
solange niemand ihn braucht. Die kleinste Angriffsfläche ist ein Prozess, der
nicht existiert.

---

## 4. Fassade und Grenzziehung

`harw-dod` ist die Fassade des Sicherheitssubsystems und wird in
`harw-dod-charter.md` begründet. Für die Integration zählt vor allem, was
**nicht** darunter liegt:

`harw-context` gehört **nicht** zu `harw-dod`. Es ist allgemeine
Infrastruktur, die jeder Agent nutzt, vom UIA bis zum Verifier. Läge es unter
der Sicherheitsfassade, würde jede Kontextänderung wie eine
Sicherheitsänderung aussehen und umgekehrt. `harw-observe` liegt aus
demselben Grund darunter im Sinne der Abhängigkeit, aber nicht darin im Sinne
der Fassade: Telemetrie ist für alle da.

Die Grenze in einem Satz: **`harw-dod` reexportiert, was mit Verteidigung zu
tun hat; `harw-context` und `harw-observe` sind Infrastruktur und stehen
daneben, nicht darunter.**

---

## 5. Prüfungen für diesen Integrationsplan

Zusätzlich zu den Teststrategien beider Einzelpläne:

**Doktrin-Gate.** Ein CI-Schritt zählt die transitiven Abhängigkeiten des
Warden-Binaries und schlägt oberhalb der Obergrenze fehl. Ein zweiter prüft,
dass im Warden-Teilbaum keine Crate mit `links`-Attribut oder `build.rs` mit
C-Compiler vorkommt.

**Disjunktheits-Test.** Ein Test lädt die Security-Ceiling und die
Scout-Ceiling und prüft, dass der Schnitt ihrer `universe`-Mengen leer ist.
Das ist S6 als Zeile.

**Gemeinsame Fixtures.** Die Injection-Fixtures liegen in einem Verzeichnis
und werden von zwei Testsuiten gelesen: der Rendering-Suite in `harw-core`
und der Triage-Suite in `harw-escalate`. Ein Fixture, zwei Prüfungen.

**Golden Renders für Sicherheitsprogramme.** Der eingefrorene Render von
`security-triage@1` ist Teil der Regression: wenn sich der Prompt eines
Sicherheitsagenten ändert, ist das eine sichtbare Änderung mit Diff, nicht
ein stiller Drift.

---

## 6. Offene Punkte dieses Plans

1. **`rustables`-Reife.** Die Doku ist ungewöhnlich offen über die rauen
   Kanten. Vorschlag: die Regelseite in `harw-netpolicy` so schneiden, dass
   der `NetPlan` rein und vollständig testbar ist und der Anwendungsteil
   klein bleibt; falls sich die Crate als zu unzuverlässig erweist, ist der
   Ersatz durch eigene Netlink-Nachrichten auf `netlink-packet-*` ein
   begrenzter Umbau hinter D4.
2. **`aya`-Version.** Vor W7 den Stand prüfen; falls 1.0 bis dahin da ist,
   direkt darauf. Bis dahin gilt D5: keine `aya`-Typen in öffentlichen
   Signaturen.
3. **Obergrenze der Warden-Deps.** Zwölf ist ein Vorschlag, keine Messung.
   Nach dem ersten Prototyp festzurren.
4. **`cargo vet`-Umfang.** Nur DoD-Teilbaum ist die Empfehlung; die
   Alternative, den ganzen Workspace zu vetten, ist bei 46 Membern
   wahrscheinlich zu teuer.
5. **Fallback ohne eBPF.** Auf Kerneln ohne `CAP_BPF` oder ohne BTF muss der
   Sentinel lauffähig bleiben, nur mit weniger Sensoren. Das ist bereits über
   `SensorHandle<Unbound>` ausdrückbar, muss aber als Betriebsfall
   dokumentiert werden.
