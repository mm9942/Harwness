# Harwness Security & Observability Subsystem, Bauplan v2

**Status:** Entwurf zur Diskussion, noch nicht normativ
**Revision:** v2, überarbeitet gegen den neuesten Workspace-Stand
**Basis:** 46 Member, 205 interne Kanten, Tiefe 8, rund 192K LOC, 3151 Tests.
Die Plan-Integration ist gelandet: `harw-plan` liegt auf L1 mit fünf
Konsumenten, `harw-plan-bridge` ist die Naht zwischen Goal, Plan, Findings und
Jobs.
**Scope:** Host-Sensorik, Sicherheits-Triage, Durchsetzung, Telemetrie,
Selbstüberwachung des Systems einschließlich seiner eigenen Struktur
**Ankerdokumente:** `docs/philosophy/philosophy.md`, `docs/philosophy/coding-philosophy.md`,
`docs/design/agent-definition-dsl.md`, `knowledge-surfaces.md`, `secrets-and-audit.md`,
`model-catalog-v2.md`, `memory-v2.md`, `planning-tool-v1.md`

---

## 0. Vorbemerkung zur Methode

Dieser Plan ist bottom-up geschnitten: er beginnt bei Vokabular-Leaves ohne
Abhängigkeiten und steigt schichtweise auf. Quer dazu ist jede Welle so
geschnitten, dass ihre Knoten parallel von fokussierten Workern bearbeitbar
sind, weil sie sich nur über bereits festgelegte Typen berühren. Die
Zell-Mechanik aus `harw-plan-bridge::cells` (Glob über `TaskId` und
`write_scope`, Partitionierung über `partition_write_sets`) ist dafür das
vorgesehene Ausführungsvehikel: dieser Plan ist so geschrieben, dass seine
Wellen als Zellen formulierbar sind.

Der Plan fügt bewusst so wenig Neues wie möglich hinzu. Der überwiegende Teil
ist Wiederverwendung: der Capability-Verband aus `harw-sandbox`, der
epistemische Layer aus `harw-memory`, die Artefakt- und Sichtbarkeitsschicht
aus `harw-knowledge`, der `MutationContract` und die Statusmatrix aus
`harw-plan`, die Lease- und Fencing-Semantik aus `harw-job-runtime` und
`harw-session-store`, die Rollen- und Ceiling-Algebra aus `harw-agent-dsl`,
und neu in v2: der Recherche-Vertrag aus `harw-research`, die
Workspace-Selbstanalyse aus `harw-code-graph` und die Reconcile-Schleife aus
`harw-plan-bridge`.

Neu bleiben: acht Crates, zwei versionierte Schemata, eine Agentenfamilie,
fünf Proc-Macros und eine Erweiterung des Permission-Vokabulars.

---

## 1. Normative Invarianten

Als Erweiterung von `agent-definition-dsl.md` §22 gedacht. Alles Weitere folgt
aus dieser Liste.

**S1.** Kein modellformulierter Inhalt erreicht jemals einen privilegierten
Ausführungspfad. Die IPC-Schnittstelle zum Durchsetzer kennt keine Operation,
die freien Text oder freie Argumente ausführt.

**S2.** Die Aktionsmenge des Durchsetzers ist geschlossen: benannte,
typisierte, validierte, auditierte Operationen. Neue Aktionen entstehen
ausschließlich durch Codeänderung im Durchsetzer, niemals durch Konfiguration,
Plugin oder Definition.

**S3.** Beobachtung und Durchsetzung leben in verschiedenen Prozessen.
Beobachtung läuft unprivilegiert mit gezielten Lesegruppen. Durchsetzung hat
keinen Modellkontakt und keine Netzwerkanbindung nach außen.

**S4.** Die Eskalationsstufe hängt an der Härte des Befunds, nicht am Urteil
eines Modells. Deterministisch entscheidbare Vertragsverletzungen dürfen
handeln, statistische Auffälligkeiten dürfen ausschließlich warnen.
Präzedenz im Workspace: `validate_goal_action` weist Akteure mit
`model:`-Präfix für terminale Goal-Übergänge bereits heute ab. Dieselbe
Actor-Konvention (`human:`, `model:`, plus neu `warden`) gilt im gesamten
Sicherheitssubsystem.

**S5.** Reversibel vor irreversibel. Freeze vor Kill. cgroup vor PID. Jede
irreversible Aktion erfordert eine menschliche Bestätigung.

**S6.** Sicherheitsbefunde sind `VisibilityScope::OperatorOnly` und verlassen
niemals die lokale Inferenz. Ein Agent mit Sicht auf Sicherheitskontext hat
keinen `NetworkAccess`. Ein Agent mit `NetworkAccess` hat keine Sicht auf
Sicherheitskontext. Die beiden Mengen sind disjunkt. Ihr einziger Treffpunkt
sind reviewte Knowledge-Artefakte.

**S7.** Audit-Checkpoints werden off-host gespiegelt. Der Meldeweg zum
Menschen ist out-of-band und funktioniert, wenn der überwachte Host nicht
funktioniert.

**S8.** Baselines tragen Provenienz und Gültigkeit. Die Beförderung eines
Normalzustands nach `Confidence::Established` ist review-gated wie jede
andere Palace-Promotion.

**S9.** Was strukturell verhindert werden kann, wird verhindert und nicht
überwacht. Überwachung ist die Antwort für das, was strukturell offen bleiben
muss.

**S10.** Telemetriefelder sind Vertrag. Feldnamen werden generiert, nicht
getippt. Ein Feld, das nicht deklariert ist, existiert nicht.

**S11.** Änderungsattribution ja, Verhaltensbewertung nein. Für menschliche
Akteure endet die Leiter bei Melden und Protokollieren; automatisches
Eingreifen nur bei deterministisch entscheidbaren
Berechtigungsverletzungen.

---

## 2. Crate-Landschaft

### 2.1 Neue Crates, dependency-geordnet

| Crate | Ebene | Abhängigkeiten (intern) | Verantwortung |
|---|---|---|---|
| `harw-observe` | L1 | `harw-types`, `harw-macros` | Telemetrie-Vokabular, Feldvertrag, Sink-Trait, Redaktion |
| `harw-signals` | L1 | `harw-types`, `harw-macros`, `harw-observe`, `harw-research` | Sicherheits- und Host-Vokabular, `Finding<S>`-Typestate, Verdict-Vertrag |
| `harw-warden-proto` | L1 | `harw-types`, `harw-macros`, `harw-signals` | Geschlossene Aktionsmenge als Wire-Typen, IPC-Rahmen |
| `harw-sensor` | L2 | `harw-signals`, `harw-observe`, `harw-sandbox`, `harw-code-graph`, `harw-home` | Reader-Backends: sysfs, procfs, fanotify, auditd, eBPF, Workspace |
| `harw-netpolicy` | L2 | `harw-sandbox`, `harw-types`, `harw-observe` | Egress-Mengen, Netzwerk-Namespace- und nftables-Plan |
| `harw-warden` | L2 | `harw-warden-proto`, `harw-observe`, `harw-netpolicy` | Privilegierter Durchsetzer, Binary, kein Modellkontakt |
| `harw-rules` | L3 | `harw-signals`, `harw-plan`, `harw-sandbox`, `harw-knowledge` | Deterministische Regel- und Vertragsschicht |
| `harw-escalate` | L4 | `harw-rules`, `harw-warden-proto`, `harw-knowledge`, `harw-plan-bridge`, `harw-observe` | Eskalationsleiter, alleiniger Konstruktor autorisierter Aktionen, Plan-Andockung |

Zusätzlich ein Ops-Modul in `harw-ops` (kein eigenes Crate) für die
Operator-Oberfläche, und Erweiterungen von `harw-sandbox`, `harw-types`,
`harw-macros`, `harw-knowledge` (§2.4).

### 2.2 Warum genau dieser Schnitt

`harw-observe` und `harw-signals` sind Leaves, weil Vokabular immer Leaf ist.
Damit sind sie von allem konsumierbar, ohne Zyklen zu erzeugen, und ihre
Typänderungen brechen sichtbar überall.

`harw-warden-proto` ist von `harw-warden` getrennt, damit der Sentinel die
Wire-Typen linken kann, ohne den privilegierten Code zu linken. Das ist die
technische Umsetzung von S3: der beobachtende Prozess enthält physisch keinen
Durchsetzungscode.

`harw-escalate` liegt oberhalb von `harw-rules`, weil nur dort der Konstruktor
für `Action<Authorized>` existiert. Adapterseitiger Agentencode hängt an
`harw-rules` und `harw-signals`, nie an `harw-escalate`. Damit ist S1 ein
Sichtbarkeitsproblem und kein Reviewpunkt. Neu in v2 hängt `harw-escalate`
zusätzlich an `harw-plan-bridge`, weil die Plan-Wirkung von Befunden
ausschließlich durch die Bridge läuft (§4.5) und nirgendwo sonst.

### 2.3 Gelandete Bausteine, die dieser Plan jetzt voraussetzt

**`harw-plan-bridge`** besitzt keinen Zustand: Plan und Goal gehören
`harw-plan`, Jobs gehören `harw-session-store`, Findings gehören
`harw-research`; die Bridge übersetzt nur und legt die Reihenfolge fest.
`PlanController::reconcile` ist eine reine Funktion mit injiziertem `now`,
alles mit Nebenwirkung liegt in `apply` und `job_bridge`, und alles, was eine
Entscheidung verlangt, wird als Vorschlag zurückgegeben statt still
ausgeführt. Terminale Goal-Übergänge wendet `apply` ausdrücklich nicht an.
Genau diese Disziplin übernimmt das Sicherheitssubsystem unverändert.

**`harw-research`** ist der normalisierte Rückgabevertrag für read-only
Sub-Agenten: gebundene Fragen (`ResearchQuestion`, `QuestionScope`),
normalisierte Ergebnisse (`ResearchFinding`, `FindingBundle`,
`SourceReference`, `VersionReference`, `Confidence`), JSON-Schema- und
Prompt-Erzeugung (`schema`), toleranter Parser (`parse_and_validate`
schneidet Markdown-Fences weg) und der reduzierte `ReturnEnvelope`. Der
Verdict-Vertrag der Triage wird nach exakt diesem Muster gebaut, und der
Intel-Scout verwendet den Vertrag unverändert (§7).

**`harw-code-graph`** liest ausschließlich `Cargo.toml` und `Cargo.lock`,
startet bewusst keinen Subprozess und ist damit für Sub-Agenten ohne
`ExecuteProcess` nutzbar. `WorkspaceGraph` mit `topological_levels`,
`parse_lockfile` und `RegistrySourceLocator` sind die Grundlage für den
`WorkspaceSensor` (§4.1): das System überwacht seine eigene Struktur mit
demselben Mechanismus, mit dem es den Host überwacht.

**Die Organisationsschicht der DSL** ist materiell:
`RawOrganizationDefinition` mit `RawRootSpec`, `RawClanSpec` und
`RawCellSpec` (`CellKind`, `CellBarrier`, `CellWritePartition`) wird zu
einer `ResolvedOrganization` aufgelöst. Ein Clan bindet einen Leader und
eine Familie an ein Plan-Revier (`plan_scope`-Glob, `child_depth_cost`),
und eine Zelle eines Clans kann keine Knoten außerhalb dieses Reviers
erhalten. Das Sicherheitssubsystem nutzt das als Container (§7.1) statt
eigene Zuständigkeitsgrenzen zu erfinden.

**`harw-core-bridge`** hostet die Kopplung zwischen Operation-SDK und
Core-Runtime: `AgentToolAdapter`, `AgentProductAdapter`,
`ChildReturnContract` (typisierte Auswertung von Kind-Rückgaben: Freitext,
`ResearchFinding`, `ReturnEnvelope`), `parse_budget_hint` mit
`tighten_budget` (Budgets können nur enger werden, noch ein Halbverband)
und `fanout_children` als der nebenläufige Fan-out-Baustein hinter
`/analyze` und dem Explore-Fan-out. Der Triage-Rückgabeweg dockt hier an
(§7.5).

**Die Operationsflächen existieren:** `/goal`, `/plan`, `/explore`,
`/research` und `/analyze` sind implementiert. `/explore` validiert gegen
den `ResearchFinding`-Vertrag und hängt das Ergebnis als `EvidenceRef` an
den Plan-Knoten; `/analyze` lädt den Workspace-Graph über
`harw-code-graph`, legt pro Crate einen Analysis-Knoten an, fährt die
Ebenen von den Blättern aufwärts als Fan-out-Wellen und verdichtet in
einem Synthesis-Knoten. Die TUI führt den `GoalStore` und rendert
`evaluate_goal`. Eine Regel aus `/analyze` wird übernommen: eine
Operation, die selbst orchestriert, trägt kein `agent_tool`-Attribut,
damit das Modell eine Orchestrierung nicht für einen Einzelaufruf hält.

### 2.4 Erweiterungen bestehender Crates

**`harw-types`** bekommt die neuen ID-Newtypes nach bestehendem Muster
(`try_from_str`, `parse`, `FromStr`, Ablehnung leerer Werte): `FindingId`,
`SensorId`, `ActionId`, `BaselineId`, `HostId`, `CgroupId`. Anmerkung zur
Abgrenzung: `harw-plan` führt seine Plan-IDs (`PlanId`, `RevisionId`,
`TaskId`, `PathOrSymbol`) bewusst lokal in `ids.rs`; dieser Plan folgt der
Ist-Konvention und erzwingt keine Verschiebung.

**`harw-sandbox`** bekommt die zweite Trägermenge des Verbands:

```rust
pub enum EgressTarget {
    Host(String),
    Cidr(IpNet),
    DnsSuffix(String),
}

pub struct EgressSet { allowed: BTreeSet<EgressTarget> }

impl EgressSet {
    pub fn empty() -> Self;
    pub fn intersection(&self, other: &Self) -> Self;
    pub fn contains(&self, target: &ResolvedTarget) -> bool;
}
```

Keine `allow`-Methode, keine `union`, kein `insert` nach Konstruktion. Exakt
die Form von `PermissionSet`. `SandboxSpec` bekommt ein Feld
`egress: EgressSet`, und die Kind-Ableitung schneidet es genauso wie die
Permissions. Damit erbt Egress die Monotonie geschenkt, und
`Permission::NetworkAccess` wird vom Schalter zum Gate vor einer Menge.

**`harw-macros`** bekommt fünf neue Makros (§5).

**`harw-ops`** bekommt `security.rs` mit den Operator-Operationen
(`OperationDomain` erhält eine neue Variante `Security`).

**`harw-knowledge`** bekommt zwei `ArtifactKind`-Varianten: `SecurityFinding`
und `Baseline`.

**`harw-plan`** bekommt eine additive Erweiterung von `EvidenceRef` (§11.2):
ein optionales Digest-Feld, serde-tolerant, damit Sicherheitsnachweise
manipulationserkennbar werden, ohne die fünf bestehenden Konsumenten zu
brechen.

**`harw-home`** liefert die Pfad-Konventionen für Reports-Verzeichnis
(Scanner), `FreezeStore` und Sample-Ring-Snapshots, damit keine neue
Pfadlogik entsteht.

---

## 3. Schicht L1: Vokabular

### 3.1 `harw-observe`

Dieses Crate löst das Problem der handgetippten Tracing-Aufrufstellen. Es
enthält keine Implementierung eines Backends, nur den Vertrag.

```rust
/// Ein deklarierter Telemetrie-Feldname. Nur über `field!` konstruierbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FieldName(&'static str);

/// Ein deklarierter Metrikschlüssel.
pub struct MetricKey {
    pub name: &'static str,
    pub kind: MetricKind,
    pub unit: Unit,
    pub labels: &'static [FieldName],
    pub cardinality: Cardinality,
}

pub enum MetricKind { Counter, Gauge, Histogram }
pub enum Unit { Count, Bytes, Seconds, Millis, Tokens, Celsius, Percent, Ratio }
pub enum Cardinality { Bounded(u32), Unbounded }
```

**Redaktion.** `harw-tools::TracedToolExecutor` hat die Regel bereits:
Argumente nie, nur Bytelänge; Output nie, nur Status. Hier wird sie zur
allgemeinen Eigenschaft:

```rust
pub trait Redact {
    fn redacted(&self) -> RedactedValue;
}

pub enum RedactedValue {
    Plain(TelemetryValue),
    Shape { len: usize },
    Digest([u8; 8]),
    Omitted,
}
```

`Redact` wird per Derive erzeugt (§5.5), Default für unbekannte Typen ist
`Omitted`. Typen aus `harw-secrets` implementieren `Omitted`, sodass sie in
Telemetrie strukturell nicht auftauchen können.

**Sink-Trait.** Der externe Telemetrie-Baustein hängt hier an und ist die
einzige Stelle, die OTel oder Prometheus kennt:

```rust
pub trait TelemetrySink: Send + Sync {
    fn record(&self, key: &MetricKey, value: TelemetryValue, labels: &LabelSet);
    fn observe(&self, key: &MetricKey, value: f64, labels: &LabelSet);
    fn health(&self) -> SinkHealth;
}

pub struct NullSink;
```

**Span-Kontext über Prozessgrenzen.**

```rust
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct TraceContext { trace_id: [u8; 16], span_id: [u8; 8], flags: u8 }
```

`TraceContext` wird mitpersistiert in `StoredJob` (`harw-job-runtime`), in
`ChildLeaseRecord` (`harw-session-store`) und im IPC-Rahmen von
`harw-warden-proto`. Ohne das zerfällt jeder durable Job und jeder
Child-Handoff in unverbundene Traces. Datenmodelländerung, deshalb frühe
Welle.

**Fehlertyp.**

```rust
#[derive(Debug, HarwError)]
pub enum ObserveError {
    #[msg("unknown metric key: {0}")] UnknownKey(String),
    #[msg("label cardinality exceeded for {0}")] CardinalityExceeded(String),
    #[msg("sink unavailable")] SinkUnavailable,
}
```

### 3.2 `harw-signals`

Vokabular für Host-Messwerte und Sicherheitsereignisse. Zwei Ströme, bewusst
getrennt: Messwerte sind Zeitreihen, Ereignisse sind Befundkandidaten.

```rust
// Strom A: Host-Messwerte

pub struct HostSample {
    pub at: jiff::Timestamp,
    pub source: SensorId,
    pub scope: SampleScope,
    pub metric: &'static MetricKey,
    pub value: f64,
}

pub enum SampleScope {
    Host,
    Cpu { core: u16 },
    Disk { device: String },
    Interface { name: String },
    Gpu { index: u8 },
    Cgroup(CgroupId),
    Process { cgroup: CgroupId, exe_digest: [u8; 8] },
}
```

```rust
// Strom B: Sicherheitsereignisse

pub struct SecurityEvent {
    pub at: jiff::Timestamp,
    pub source: SensorId,
    pub host: HostId,
    pub kind: EventKind,
    pub actor: Actor,
    pub trace: Option<TraceContext>,
}

pub enum EventKind {
    ProcessExec { cgroup: CgroupId, exe: PathBuf, argv_len: usize, parent: CgroupId },
    ProcessExit { cgroup: CgroupId, code: i32 },
    FileModified { path: PathBuf, before: Option<ContentDigest>, after: ContentDigest },
    ConfigDrift { path: PathBuf, diff: UnifiedDiff },
    StructureDrift { change: WorkspaceChange },
    AuthAttempt { service: Service, outcome: AuthOutcome, remote: IpAddr, method: AuthMethod },
    ListenerOpened { addr: SocketAddr, cgroup: CgroupId },
    EgressFlow { flow: FlowKey, bytes: u64, sni: Option<String>, cgroup: Option<CgroupId> },
    ScanReport { tool: ScanTool, findings: Vec<ScanFinding> },
    ThresholdBreach { metric: &'static MetricKey, scope: SampleScope, value: f64, limit: f64 },
}

/// Struktur-Drift des eigenen Workspaces, aus `harw-code-graph` abgeleitet.
pub enum WorkspaceChange {
    DependencyAdded { package: String, version: String, direct: bool },
    DependencyRemoved { package: String },
    VersionChanged { package: String, from: String, to: String },
    MemberAdded { member: String },
    EdgeAdded { from: String, to: String },
}

/// Actor-Konvention wie in `EvidenceRef` bereits etabliert
/// (`human:mia`, `worker-abc`), erweitert um `warden`.
pub enum Actor {
    Human { auid: u32, session: Option<u32> },
    Agent { session: SessionId, role: AgentRoleId, node: Option<String> },
    System { unit: String },
    Warden,
    Unknown,
}
```

**Typestate für Befunde.**

```rust
pub struct Raw;
pub struct RuleChecked;
pub struct Triaged;

pub struct Finding<S> {
    id: FindingId,
    event: SecurityEvent,
    hardness: Hardness,
    severity: Severity,
    evidence: Vec<SecurityEvidence>,
    verdict: Option<Verdict>,
    _state: PhantomData<S>,
}

pub enum Hardness {
    ContractViolation,
    RuleTriggered,
    Anomaly,
    Informational,
}

pub enum Severity { Info, Low, Medium, High, Critical }
```

Konstruktoren so geschnitten, dass Zustände nicht übersprungen werden können:
`Finding::<Raw>::new` ist `pub(crate)` in `harw-sensor`,
`Finding::<Raw>::check` lebt in `harw-rules`, `Finding::<RuleChecked>::triage`
in `harw-escalate`. Ein Agent kann `Finding<Triaged>` nicht selbst
herstellen.

**Verdict-Vertrag nach `harw-research`-Muster.** Neu in v2: statt einen
eigenen Vertrag zu erfinden, wird die Maschinerie gespiegelt, die es schon
gibt. `harw-signals::verdict` bekommt wie `harw-research` ein `schema`-Modul
(`verdict_json_schema()`, `verdict_schema_prompt()`) und ein
`validate`-Modul (`parse_and_validate`, fence-tolerant), weil lokale Modelle
Fences emittieren, egal was man ihnen sagt. Primärweg bleibt der strikte
Tool-Call, der tolerante Parser ist der Fallback für schwache Modelle.

```rust
pub struct Verdict {
    pub finding: FindingId,
    pub assessment: Assessment,
    /// Bewusst `harw_research::Confidence`, kein drittes Konfidenz-Vokabular.
    pub confidence: harw_research::Confidence,
    pub rationale: String,
    pub evidence: Vec<SecurityEvidence>,
    pub proposed: Option<ProposedAction>,
}

pub enum Assessment { Benign, NeedsReview, Suspicious, Malicious }

/// Spiegelt exakt die Warden-Aktionsmenge, erzeugt aus derselben
/// Makro-Deklaration (§5.4).
pub enum ProposedAction {
    Observe,
    Notify { channel: NotifyChannel },
    FreezeCgroup { cgroup: CgroupId, ttl_seconds: u32 },
    BlockEgress { flow: FlowKey, ttl_seconds: u32 },
    RevokeGrant { grant: GrantId },
}
```

Die Abbildung von `harw_research::Confidence` auf
`harw_memory::epistemic::Confidence` passiert an genau einer Stelle, beim
Baseline-Kurator im Promotionsvorschlag, analog zur Zeitachsen-Naht
`offset_from_timestamp`: ein Vokabularpaar, eine benannte
Konvertierungsfunktion, nirgends sonst.

**Sicherheitsnachweis.**

```rust
/// Nachweis mit Inhalts-Digest. Wird über die additive `EvidenceRef`-
/// Erweiterung (§11.2) an Plan-Knoten heftbar.
pub struct SecurityEvidence {
    pub locator: String,
    pub digest: ContentDigest,      // blake3, wie SnapshotId
    pub produced_at: jiff::Timestamp,
    pub actor: String,              // Actor-Konvention
}
```

**Fehlertyp.**

```rust
#[derive(Debug, HarwError)]
pub enum SignalError {
    #[msg("event schema version {0} not supported")] UnsupportedSchema(String),
    #[msg("evidence reference {0} could not be resolved")] DanglingEvidence(String),
    #[msg("severity/hardness combination is not admissible: {0:?}/{1:?}")]
    InadmissibleCombination(Severity, Hardness),
}
```

### 3.3 `harw-warden-proto`

Ausschließlich Wire-Typen. Keine Logik, keine Privilegien, kein Modell.

```rust
/// Schema `harwness.warden/v1`.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WardenRequest {
    pub schema: SchemaVersion,
    pub action_id: ActionId,
    pub issued_at: jiff::Timestamp,
    pub trace: TraceContext,
    pub finding: FindingId,
    pub action: WardenAction,
    pub authorization: AuthorizationProof,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "op")]
pub enum WardenAction {
    FreezeCgroup { cgroup: CgroupId, ttl_seconds: u32 },
    ThawCgroup { cgroup: CgroupId },
    BlockEgress { flow: FlowKey, ttl_seconds: u32 },
    UnblockEgress { flow: FlowKey },
    RevokeGrant { grant: GrantId },
}
```

Bewusst nicht enthalten: `Exec`, `Kill`, `WriteFile`, `RunScript`, irgendein
Feld, das als Befehl interpretiert wird. S2 ist damit strukturell und nicht
dokumentarisch.

`AuthorizationProof` trägt die Herkunft der Autorisierung (Regelschicht oder
Operator-Bestätigung) mit Zeitstempel und, bei Operator-Bestätigung, der
Approval-ID aus `harw-session-store::ApprovalStore`. Der Warden prüft sie
unabhängig nach und vertraut dem Absender nicht.

---

## 4. Schicht L2 bis L4: Verarbeitung

### 4.1 `harw-sensor`

Backends hinter einem Trait, wie die Provider. Jedes Backend deklariert seine
Berechtigung und wird bei fehlender Berechtigung nicht geladen statt zur
Laufzeit zu scheitern.

```rust
pub trait Sensor: Send + Sync {
    fn id(&self) -> SensorId;
    fn required(&self) -> SensorCapability;
    fn cadence(&self) -> Cadence;
    fn poll(&self, out: &mut SampleSink) -> SensorResult<()>;
    fn subscribe(&self, out: EventTx) -> SensorResult<Subscription>;
}

pub enum SensorCapability {
    SysfsRead,
    ProcfsRead,
    JournalRead,
    AuditNetlink,
    FanotifyMark,
    BpfLoad,
    ReportsDirRead,
    WorkspaceRead,   // neu: Cargo.toml/Cargo.lock, kein Subprozess
}
```

**Typestate für Sensoren:**

```rust
pub struct Unbound;
pub struct Bound;

pub struct SensorHandle<S> { /* ... */ _s: PhantomData<S> }

impl SensorHandle<Unbound> {
    /// Prüft die Capability real (Probe-Read) und bindet erst dann.
    pub fn bind(self) -> SensorResult<SensorHandle<Bound>>;
}
```

Nur `SensorHandle<Bound>` hat `poll` und `subscribe`.

**Konkrete Backends:**

| Backend | Quelle | Capability | Strom |
|---|---|---|---|
| `HwmonSensor` | `/sys/class/hwmon`, `/sys/class/thermal` | SysfsRead | A |
| `CpuSensor` | `/proc/stat`, cpufreq, thermal throttle | ProcfsRead | A |
| `MemorySensor` | `/proc/meminfo`, `/proc/pressure/memory` | ProcfsRead | A |
| `IoSensor` | `/proc/diskstats`, `/proc/pressure/io`, statvfs | ProcfsRead | A |
| `NetSensor` | `/proc/net/dev`, conntrack count | ProcfsRead | A |
| `GpuSensor` | NVML, `/sys/class/drm`, hwmon | SysfsRead | A |
| `CgroupSensor` | `cpu.stat`, `memory.current`, `io.stat`, `pids.current` | SysfsRead | A |
| `ProcessSensor` | eBPF `sched_process_exec`/`exit`, procfs-Abgleich | BpfLoad | B |
| `AuthSensor` | auditd `USER_AUTH`/`USER_LOGIN`, journald sshd, btmp | AuditNetlink | B |
| `FileSensor` | fanotify `FAN_MODIFY`/`FAN_CLOSE_WRITE` + loginuid | FanotifyMark | B |
| `ListenerSensor` | `/proc/net/tcp{,6}` plus Inode-Zuordnung | ProcfsRead | B |
| `FlowSensor` | eBPF/XDP Flow-Aggregation, SNI, DNS | BpfLoad | B |
| `ScanSensor` | Reports von rkhunter, ClamAV, Lynis, AIDE, smartctl | ReportsDirRead | B |
| `WorkspaceSensor` | `harw-code-graph`: `WorkspaceGraph`, `parse_lockfile` | WorkspaceRead | B |

**`WorkspaceSensor`, neu in v2.** Das System überwacht seine eigene Struktur:
periodischer `WorkspaceGraph::load` plus `parse_lockfile`, Diff gegen den
letzten abgenommenen Stand, jede Abweichung ist ein
`EventKind::StructureDrift`. Eine neue Dependency, ein Versionssprung, eine
neue interne Kante, ein neues Member. Attribution über den `FileSensor`
(loginuid auf `Cargo.toml`/`Cargo.lock`), Abgleich gegen das kuratierte
Crate-Inventar in den Docs als Vertragsgrundlage: eine Dependency, die nicht
im Inventar deklariert ist, ist `RuleTriggered`, nicht bloß `Anomaly`.
Zusammen mit dem Advisory-Weg über den Intel-Scout (§7.3) ergibt das eine
Supply-Chain-Pipeline, die S6 respektiert: lokal wird nur gelesen, Netz hat
nur der Scout, der Join passiert in der Regelschicht über reviewte Artefakte.

**Scanner-Regel.** Der Sensor führt die Werkzeuge nicht aus. Ein
systemd-Timer mit festen Argumenten schreibt Reports in das
Reports-Verzeichnis unter `harw-home`, der Sensor liest nur. Es gibt keine
Stelle, an der eine Kommandozeile zusammengebaut wird.

**Ringpuffer und Evidence-Snapshot.**

```rust
pub struct SampleRing { window: Duration, entries: VecDeque<HostSample> }

impl SampleRing {
    /// Friert die letzten `window` Sekunden ein und liefert eine
    /// digest-tragende `SecurityEvidence` für `Finding::evidence`.
    pub fn freeze(&self, at: Timestamp) -> SecurityEvidence;
}
```

Hochauflösend sammeln, verdichtet weitergeben, bei einem Befund das Fenster
einfrieren und anhängen. Dieselbe Logik wie der Diary-Append bei Compaction:
der Übergang ist das Schreibereignis, nicht der Dauerzustand.

### 4.2 `harw-netpolicy`

Deklaration und Durchsetzung der Egress-Menge, getrennt. Deklaration ist
rein, Durchsetzung braucht Privilegien und lebt nur im Warden.

```rust
/// Reiner Plan, inspizierbar, ohne Seiteneffekt. Analog `BwrapCommandPlan`.
pub struct NetPlan {
    pub netns: NetnsName,
    pub rules: Vec<NftRule>,
    pub resolver: ResolverPolicy,
}

pub fn plan_for(spec: &SandboxSpec) -> NetPolicyResult<NetPlan>;
```

Der Plan ist ohne Root testbar, exakt wie der bwrap-Plan heute. Die Anwendung
(netlink, nftables) liegt im Warden.

### 4.3 `harw-rules`

Deterministische Schicht. Kein Modell, kein Netz, reine Funktionen über
Referenzen mit injiziertem `now`, exakt die Reinheitsdisziplin von
`PlanController::reconcile`: zweimal auf demselben Zustand aufgerufen liefert
sie dieselben Treffer.

```rust
pub trait Rule: Send + Sync {
    fn id(&self) -> RuleId;
    fn hardness(&self) -> Hardness;
    fn evaluate(&self, ev: &SecurityEvent, ctx: &RuleContext<'_>) -> Option<RuleHit>;
}

pub struct RuleContext<'a> {
    pub baselines: &'a BaselineIndex,
    pub contracts: &'a ContractIndex,
    pub recent: &'a [SecurityEvent],
    pub now: Timestamp,
}
```

**Die drei Regelklassen, nach Härte:**

1. **Vertragsregeln** (`ContractViolation`). Wiederverwendung von
   `harw_plan::admission::validate_patch` als zweite Quelle: ein
   `FileModified` mit `before`/`after` wird zum `UnifiedDiff` und gegen den
   `MutationContract` geprüft, der für diesen Pfad aktiv ist; der
   `ContractIndex` wird aus den aktiven Plan-Knoten über
   `contract_from_node` gespeist. Zweitens: `EgressFlow` mit einer `cgroup`,
   deren `EgressSet` das Ziel nicht enthält. Drittens: `ProcessExec` unter
   einer cgroup, deren Rolle nach `can_spawn` keine Kinder erzeugen darf.
   Viertens, neu: `StructureDrift` mit einer Dependency außerhalb des
   kuratierten Inventars.

2. **Schwellenregeln** (`RuleTriggered`). PSI-Druck, Temperatur,
   Auth-Fehlversuchsrate, Plattenfüllstand. Grenzen kommen aus Baselines,
   nicht aus Konstanten.

3. **Abweichungsregeln** (`Anomaly`). Erster erfolgreicher Login eines Kontos
   von unbekannter Quelle, Beacon-Periodizität, Egress-Volumen über Baseline,
   neuer Listener, unbekannte Bibliothek in laufendem Prozess.

**Baselines.** Ein `Baseline`-Artefakt in `harw-knowledge` mit
Palace-Semantik:

```rust
pub struct Baseline {
    pub id: BaselineId,
    pub host: HostId,
    pub subject: BaselineSubject,
    pub expectation: Expectation,
    pub provenance: harw_memory::epistemic::Provenance,
    pub confidence: PalaceConfidence,   // Established | Provisional | Superseded
    pub validity: harw_memory::epistemic::Validity,
    pub superseded_by: Option<BaselineId>,
}

pub enum Expectation {
    NumericRange { min: f64, max: f64 },
    AllowedSet(BTreeSet<String>),
    Periodicity { period: Duration, tolerance: Duration },
    DeclaredState(ContentDigest),
}
```

`Established` darf `RuleTriggered` erzeugen, `Provisional` nur `Anomaly`.
Eine nicht abgenommene Baseline kann nach S4 keine Handlung auslösen; S8 ist
damit mechanisch wirksam.

### 4.4 `harw-escalate`

Die Leiter, und der einzige Ort, an dem Autorisierung entsteht.

```rust
pub struct Proposed;
pub struct Authorized;

pub struct Action<S> {
    id: ActionId,
    finding: FindingId,
    action: WardenAction,
    proof: Option<AuthorizationProof>,
    _s: PhantomData<S>,
}

impl Action<Proposed> {
    /// Der einzige Übergang. `pub(crate)` in diesem Crate, deshalb von
    /// Agenten-Adaptern nicht erreichbar.
    pub(crate) fn authorize(self, by: Authority, ladder: &Ladder)
        -> EscalateResult<Action<Authorized>>;
}

pub async fn dispatch(action: Action<Authorized>, client: &WardenClient)
    -> EscalateResult<ActionReceipt>;
```

**Die Leiter ist fest verdrahtet und nicht konfigurierbar:**

| Stufe | Zulässig ab Hardness | Autorität |
|---|---|---|
| Observe | alle | Regelschicht |
| Record | alle | Regelschicht |
| Notify (in-band) | RuleTriggered | Regelschicht |
| Notify (out-of-band) | High/Critical bei RuleTriggered | Regelschicht |
| FreezeCgroup / BlockEgress | ContractViolation | Regelschicht |
| Kill, Rollback, Revoke | niemals automatisch | Mensch |

Ein `Anomaly`-Befund kann Freeze strukturell nicht erreichen, weil
`Ladder::admissible(hardness, step)` es ablehnt und `authorize` ohne
Zulässigkeit keinen `Action<Authorized>` konstruiert.

**Freeze ist ein Lease.** Semantik aus
`harw-session-store::ChildLeaseStore` wiederverwendet:

```rust
pub struct FreezeActive;
pub struct FreezeResolved;

pub struct Freeze<S> {
    cgroup: CgroupId,
    finding: FindingId,
    frozen_at: Timestamp,
    expires_at: Timestamp,
    _s: PhantomData<S>,
}

impl Freeze<FreezeActive> {
    pub fn release(self, by: Authority) -> Freeze<FreezeResolved>;
    pub fn escalate(self, by: Authority) -> Freeze<FreezeResolved>;
}

pub fn reconcile_expired_freezes(store: &FreezeStore, now: Timestamp)
    -> Vec<ExpiredFreeze>;
```

Ein eingefrorener Prozess, den alle vergessen, ist strukturell unmöglich: der
Ablauf legt ihn zurück auf die Leiter, wie `reconcile_expired_leases` beim
Start.

### 4.5 Andocken an die Plan-Schleife, neu in v2

Die Bridge existiert jetzt, also läuft jede Plan-Wirkung eines Befunds
ausschließlich durch sie. Das Sicherheitssubsystem besitzt keinen
Plan-Zustand, exakt wie die Bridge selbst nichts besitzt.

**Befund als Evidenz.** Betrifft ein Befund einen Pfad, der im `write_scope`
eines aktiven Plan-Knotens liegt, wird er als Nachweis an diesen Knoten
geheftet, über die bestehende `AttachEvidence`-Konvention. Vorbild ist
`finding_store::evidence_for_finding`, das aus einem `ResearchFinding` einen
`EvidenceRef` baut; das Gegenstück
`evidence_for_security_finding(plan_id, finding, actor)` lebt in
`harw-escalate` und nutzt die Zeitnaht `offset_from_timestamp` aus der
Bridge, keine eigene Konvertierung (§11.1).

**Vertragsverletzung als Invalidierung.** Ein `ContractViolation`-Befund auf
einem Knotenpfad erzeugt einen Vorschlag `ReconcileStep::Invalidate` für die
abhängigen Knoten, mit dem Befund als Begründung. Vorschlag, nicht stille
Ausführung: `apply` entscheidet in der bestehenden Schleife, der Operator
sieht es in der bestehenden Oberfläche.

**Anomalie als Explorationsvorstufe.** Ein `Anomaly`-Befund auf dem Zielpfad
eines noch nicht gestarteten, riskanten Knotens kann
`ReconcileStep::InsertExplore` vorschlagen: erst nachsehen, dann schreiben.
Dieselbe Mechanik, die die Bridge für riskante Arbeit ohnehin vorsieht.

**Terminale Übergänge bleiben menschlich.** `validate_goal_action` weist
`model:`-Akteure für `Achieved` und `Abandoned` heute schon ab. Kein Teil des
Sicherheitssubsystems bekommt einen Weg daran vorbei; der Incident-Korrelator
schreibt Berichte, keine Goal-Zustände.

**Sicherheitsarbeit als Plan-Arbeit.** Der Controller kennt neun Schritte,
nicht fünf: neben `AttachEvidence`, `Invalidate` und `InsertExplore` auch
`ProposeExpand`, `ProposeCondense`, `MarkReady`, `AdmitJobs`, `GoalStatus`
und `AskModel`. Zwei davon machen einen eigenen Security-Scheduler
überflüssig: wiederkehrende agentische Sicherheitsarbeit (Triage-Batches,
Baseline-Kuration, der Layer-4-Schreiber, die periodische Kettenprüfung)
wird als Plan-Knoten geführt, `MarkReady` erklärt sie ausführbar,
`AdmitJobs` reicht sie an die Job-Bridge, und die Job-Evidenz schließt den
Knoten nach der bestehenden Konvention ab (`EvidenceKind::Job` bedeutet
erledigt, `apply` setzt `Completed` im selben Schritt). Sentinel und Warden
bleiben als Prozesse außerhalb (systemd), aber alles Agentische fährt auf
der vorhandenen Schiene.

**`AskModel` als Präzedenz.** Der Controller stellt Fragen, die er nicht
selbst entscheiden darf, als vollständigen, präzisen Prompt an das Modell,
statt still zu raten. Exakt diese Grenze zieht die Triage: die Regelschicht
entscheidet Mechanik, das Modell bekommt gebundene Fragen mit gebundenem
Antwortvertrag. Auch die Reihenfolge-Konvention wird übernommen: Evidenz
vor Invalidierung vor Bereitschaft.

**Invarianten reiten auf dem Goal.** `Goal` führt `Invariant`- und
`Constraint`-Einträge, und der `GoalContextProvider` liefert Zielsatz,
offene Kriterien und Invarianten in jedem Turn frisch aus den Stores. Die
S-Invarianten aus §1 werden deshalb als Goal-Invarianten des
Sicherheits-Goals registriert: jeder Turn eines Security-Agenten trägt
sie, sie überleben Compaction und Modellwechsel, und die Coverage aus
`evaluate_goal` hält die Kriterienabdeckung sichtbar.

**Fan-out über Zellen.** Die parallele Triage vieler Befunde ist eine Zelle:
`members_from_plan` wählt die Triage-Knoten, `write_partition = None`, weil
Triage-Worker nichts schreiben; die Zelle löst zu `FanoutRequest`s auf und
läuft über `fanout_children` aus `harw-core-bridge`. Kein eigener
Worker-Pool, keine neue Fan-out-Mechanik. Kind-Budgets kommen über
`tighten_budget`, das Budgets nur verschärfen kann, derselbe Halbverband
wie überall.

---

## 5. Makros und Proc-Macros

Alle fünf in `harw-macros`, jedes mit trybuild-Korpus nach dem Muster der
sieben `FromRawArgs`-Fälle.

### 5.1 `metrics!` (function-like)

```rust
metrics! {
    CPU_UTIL: gauge, unit = Percent, labels = [core], cardinality = 256;
    TURN_TOKENS: counter, unit = Tokens, labels = [role, model], cardinality = 64;
    FREEZE_ACTIVE: gauge, unit = Count, labels = [], cardinality = 1;
    WARDEN_UNAUDITED: counter, unit = Count, labels = [], cardinality = 1;
}
```

Erzeugt `pub static`-Schlüssel und ein Registrierungs-Slice, Compile-Fehler
bei Doppelnamen und bei Labels, die nicht per `field!` deklariert sind.

### 5.2 `#[traced]` (attribute auf `impl`-Blöcken)

Instrumentiert alle `pub`-Methoden. Drei Eigenschaften, die man von Hand
regelmäßig falsch macht: async korrekt über `Instrument` statt `enter`,
Feldausdrücke lazy hinter der Aktivierungsprüfung, Argumente über `Redact`
statt `Debug`. Level und Feldauswahl kommen, wo vorhanden, aus
`OperationMeta` (`OperationDomain`, `PermissionTier`).

### 5.3 `#[derive(SensorSource)]`

Erzeugt aus einer annotierten Struktur `poll`, Metrik-Emission und
Capability-Deklaration. Ein neuer Sensor ist eine Datendeklaration statt
einer Schleife.

### 5.4 `warden_actions!` (function-like)

Die wichtigste der fünf. Deklariert die geschlossene Aktionsmenge einmal und
erzeugt: das Wire-Enum in `harw-warden-proto`
(`deny_unknown_fields`), das Vorschlags-Enum in `harw-signals`, das
Tool-JSON-Schema für die Triage (`strict: true`), die Validierung im Warden,
den Audit-Eintragstyp pro Aktion und das Skelett der
Zulässigkeitsmatrix mit Pflichteintrag pro Aktion.

```rust
warden_actions! {
    FreezeCgroup { cgroup: CgroupId, ttl_seconds: u32 }
        requires ContractViolation, reversible, audit = "freeze";
    BlockEgress { flow: FlowKey, ttl_seconds: u32 }
        requires ContractViolation, reversible, audit = "egress-block";
    RevokeGrant { grant: GrantId }
        requires Human, irreversible, audit = "grant-revoke";
}
```

Vorschlagsmenge, Ausführungsmenge, Modellschema, Validierung und Audit können
nicht auseinanderdriften, weil sie eine Quelle haben. Eine Aktion ohne
Zulässigkeitsregel oder ohne Audit-Namen kompiliert nicht.

### 5.5 `#[derive(Redact)]`

Feldweise Redaktionsstrategie, Default `Omitted`, damit Vergessen die sichere
Richtung hat.

```rust
#[derive(Redact)]
struct AuthAttempt {
    #[redact(plain)]  outcome: AuthOutcome,
    #[redact(digest)] remote: IpAddr,
    #[redact(omit)]   raw_line: String,
}
```

---

## 6. Observability und Selbstüberwachung

### 6.1 Die Span-Hierarchie

Die Kette, die ein Betreiber sehen will, ist die Kette, die Harwness ohnehin
führt, seit der Plan-Integration einschließlich Goal:

```
goal (goal_statement)
└─ plan_revision (parent_revision-Kette)
   └─ plan_node (TaskId)
      └─ reconcile_step (AttachEvidence | Invalidate | InsertExplore | ...)
      └─ job (WorkId, LeaseToken)
         └─ session (SessionId)
            └─ turn (TurnId)
               ├─ context_assembly
               ├─ model_request (provider, model, role)
               └─ tool_call (ToolName)
                  └─ child_session (Handoff)
```

Drei Stellen persistieren `TraceContext`, weil der Kontext dort Prozess- oder
Zeitgrenzen überschreitet: `StoredJob`, `ChildLeaseRecord`, `WardenRequest`.

### 6.2 Metriken, die nur Harwness haben kann

**Kontext und Kosten pro Knoten.** `context_bytes_included`,
`context_fragments_omitted`, `history_items_dropped` (liegen als
`ContextAssembly` bereits vor), `stm_evictions`, `signals_scanned` gegen
`signals_selected` aus `SelectionResult`, `tokens_in`/`tokens_out`/`cached`
aus `TokenUsage`, gelabelt mit Rolle und Modell.

**Agentenökonomie über zwei Achsen.** Tokens pro Plan-Knoten existieren;
dazu über die cgroup-Achse CPU-Sekunden, IO und Speicher pro Knoten. Damit
ist der wirkliche Preis eines Plan-Knotens erstmals eine Messung über beide
Achsen.

**Orchestrierungsgesundheit.** Aktive Kinder pro Parent gegen
`max_active_children_per_parent`, Tiefe gegen `max_depth`, abgelaufene
Leases, Reconciliations beim Start, abgelehnte Spawns nach `can_spawn`,
Zombie-Reaps.

**Plan-Schleife, neu in v2.** `reconcile_steps_total` nach Schrittart,
`invalidations_total` nach `InvalidationCondition`, `explore_inserted_total`,
`evidence_attached_total` nach Evidenzart, `proposals_pending` (Expand,
Condense, GoalStatus), `goal_age_days`, Coverage aus `evaluate_goal` als
Gauge. Und weiterhin die wertvollste Einzelmetrik: die
Scope-Violation-Rate aus `validate_patch` pro Zeitfenster. Sie steigt, wenn
die Pläne schlechter werden, bevor ein Test rot wird.

**Struktur-Selbstbild, neu in v2.** Aus dem `WorkspaceSensor`:
`workspace_members`, `workspace_edges`, `workspace_depth`,
`structure_drift_total` nach `WorkspaceChange`-Art. Das Systembild, gegen das
jede Bottom-up-Analyse startet, ist damit eine live gemessene Größe.

### 6.3 Meta-Invarianten als Nullzähler

Für jede zur Laufzeit verletzbare Invariante ein Zähler mit Erwartungswert
dauerhaft null. Ein Alarm auf ungleich null ist ein Alarm auf eine gebrochene
Invariante, nicht auf einen Schwellwert.

| Zähler | Bedeutung bei > 0 |
|---|---|
| `warden_unaudited_actions` | S2 verletzt: Aktion ohne Audit-Eintrag |
| `warden_rejected_schema` | fremde Protokollversion am Socket |
| `escalate_inadmissible` | S4 verletzt: Stufe für Hardness nicht zulässig |
| `sensor_capability_denied` | S3 verletzt: Sensor mit falschen Rechten |
| `security_context_egress` | S6 verletzt: Sicherheitsartefakt in Netz-Kontext |
| `goal_status_applied_by_model` | S4-Präzedenz verletzt: terminaler Goal-Übergang durch Modell-Akteur angewendet |
| `audit_chain_break` | S7: Kette gebrochen, Manipulationsverdacht |

`spawn_denied_by_matrix` und `goal_status_rejected_by_validation` sind
bewusst keine Nullzähler: Ablehnungen sind der Mechanismus bei der Arbeit
und dürfen vorkommen; ihre Rate ist ein Gesundheitssignal, kein Bruch.

`harw-secrets::audit::chain::verify` läuft als periodischer Job mit Metrik:
kontinuierliche Integritätsprüfung der eigenen Historie.

### 6.4 Der Rückkanal in den Modellkatalog

`ObservedModelBehavior` ist gebaut und leer: alle sechs Scores auf
`Score::HALF`, `updated_at` `None`, `evidence` leer. Die Telemetrie liefert
die Größen für alle sechs Achsen:

| Achse | Messbar aus |
|---|---|
| `tool_schema_reliability` | Anteil valider Tool-Calls gegen Schema-Fehler, inkl. Fence-Fallback-Quote aus `parse_and_validate` |
| `long_context_retention` | Fehlerrate bei Turns oberhalb Kontextschwelle |
| `delegation_discipline` | Anteil Spawns innerhalb Limits, Tiefe, Fan-out |
| `recovery_after_tool_error` | Erfolgsquote im Turn nach einem Tool-Fehler |
| `completion_calibration` | Anteil als erledigt gemeldeter Knoten, deren `Criterion` mit `EvidenceRef` belegt ist |
| `compaction_resilience` | Qualitätsdelta vor gegen nach Compaction, `goal_context` als Konstante |

Der Schreiber ist ein durabler Job im Dream-Muster: er wertet Fenster aus und
schlägt eine Aktualisierung vor. `completion_calibration` fällt seit der
Plan-Integration fast gratis ab, weil `AttachEvidence` und `evaluate_goal`
die Belegquote bereits führen. Ab dem Moment routet
`harw-model-catalog::router::pick` nach gemessenem Verhalten im eigenen
Workspace, und Downrouting auf offene Gewichte ist eine Messung statt einer
Hoffnung.

---

## 7. Agentenschicht

### 7.1 Familie und Ceiling

`harwness.family.security@1` mit einer Ceiling ohne `NetworkAccess`, ohne
`WriteWorkspace`, ohne `ExecuteProcess`. Ein Mixin
`harwness.mixin.security-readonly@1` entfernt Shell- und Schreibtools. S6 ist
eine Mengenaussage, die der Resolver prüft.

Die Familie bekommt ihren organisatorischen Container: ein
**Security-Clan** in der Organisationsdefinition, `plan_scope =
"security/*"`, Leader ist der `incident-correlator@1`, Familie ist
`harwness.family.security@1`. Eine Zelle dieses Clans kann strukturell
keine Knoten außerhalb von `security/*` erhalten, und kein anderer Clan
vergibt Sicherheitsknoten. Der `intel-scout@1` steht bewusst außerhalb des
Clans: S6 ist damit nicht nur eine Ceiling-Aussage, sondern auch
Organisationsstruktur.

Vier Spezialisierungen, alle Rolle `Worker`:

| Spezialisierung | Eingabe | Ausgabe | Modellrolle |
|---|---|---|---|
| `security-triage@1` | `Finding<RuleChecked>` + Baselines + verwandte Historie | `Verdict` (Tool-Call, strict) | Verifier |
| `baseline-curator@1` | Messfenster + bestehende Baselines | Palace-Promotion-Vorschläge | Scout |
| `drift-explainer@1` | Diff + Vertrag + Actor | Erklärung, gedeckt ja/nein | FocusedCodingWorker |
| `incident-correlator@1` | Befundfenster über Tage | `OperatorOnly`-Artefakt | Orchestrator |

### 7.2 Kontextpolicy

`harwness.context.security-triage@1`:

```toml
[budget]
tokens = 6000
reserved_for_output = 1500

[must_include]
items = ["finding.event", "finding.hardness", "contract.declared",
         "baseline.matching", "host.identity"]

[should_include]
items = ["findings.related_recent", "actor.history_summary"]

[may_include]
items = ["metrics.window_summary"]

[exclude]
items = ["full_parent_transcript", "sibling_transcripts",
         "other_hosts.findings", "secrets.any", "raw_sample_ring"]

[details]
mode = "references"
load_on_demand = true
```

Referenzbasiert, gedeckelt durch `MAX_RECALL_ARTIFACTS` und
`MAX_RECALL_HOPS`; ein Triage-Turn bleibt bei wenigen tausend Token, die
Voraussetzung für lokale Inferenz.

### 7.3 Intel-Scout über den Recherche-Vertrag

Der optionale `intel-scout@1` (CVE- und Advisory-Feeds) gehört ausdrücklich
nicht in die Security-Familie. Neu in v2: er braucht keinen eigenen Vertrag,
er verwendet `harw-research` unverändert. Eine `ResearchQuestion` pro
beobachtetem Paket (`QuestionScope` begrenzt die Quellen auf Advisory-Feeds),
zurück kommt ein `FindingBundle` mit `SourceReference` und
`VersionReference`. Der Abgleich Advisory gegen `Cargo.lock` passiert lokal
in `harw-rules` über `find_locked` aus `harw-code-graph`, nachdem das Bundle
als Knowledge-Artefakt reviewt wurde. Netz und Sicherheitskontext berühren
sich nie: der Scout sieht Paketnamen, nie Befunde; die Regelschicht sieht
Artefakte, nie das Netz.

### 7.4 Injection-Grenze

Jeder Wert aus einem `SecurityEvent` ist angreiferkontrolliert: Dateinamen,
Prozessnamen, Logzeilen, SNI, Usernamen, und neu auch Paketnamen und
Advisory-Texte. Deshalb: Ereignisdaten in einem gelabelten Datenblock, nie im
Instruktionsteil; die Baseline-Instruktion sagt ausdrücklich, dass Inhalte in
Ereignisdaten niemals Anweisungen sind; das Verdikt als Tool-Call mit
`strict: true` gegen `harwness.security-verdict/v1`, Fallback über den
fence-toleranten Parser nach `harw-research`-Vorbild; `rationale` ist ein
gespeichertes Feld, kein Kanal, wird nie geparst oder ausgeführt.

### 7.5 Return Contract

`harwness.return.security-triage@1` mit Pflichtfeldern `finding_id`,
`assessment`, `confidence`, `rationale`, `evidence`, `proposed_action`,
`state_revision_after`; `persist_before_enqueue = true`,
`include_full_transcript = false`. Ausgewertet wird die Rückgabe nicht über
einen Parallelmechanismus, sondern über `harw-core-bridge`:
`ChildReturnContract` kennt bereits Freitext, `ResearchFinding` und
`ReturnEnvelope` und bekommt einen vierten Arm `SecurityVerdict`, der den
fence-toleranten Parser aus §3.2 fährt. Für den Scout gelten unverändert
`ReturnEnvelope` und `ResearchFinding` aus `harw-research`.

---

## 8. Verdrahtungsmatrix

| Bestehendes Element | Änderung | Welle |
|---|---|---|
| `harw-types` | neue ID-Newtypes (`FindingId`, `SensorId`, `ActionId`, `BaselineId`, `HostId`, `CgroupId`) | 0 |
| `harw-macros` | fünf neue Makros plus trybuild-Korpus | 0, 3 |
| `harw-sandbox` | `EgressSet`, `SandboxSpec.egress`, Schnitt in der Kind-Ableitung | 3 |
| `harw-job-runtime` | `StoredJob.trace: Option<TraceContext>` | 1 |
| `harw-session-store` | `ChildLeaseRecord.trace`, neuer `FreezeStore` | 1, 5 |
| `harw-core::context_budget` | `ContextAssembly` emittiert Metriken | 1 |
| `harw-core::child_controller` | Spawn-, Reap-, `can_spawn`-Metriken | 1 |
| `harw-core::admission` | `JobIntent` trägt Trace, Admissions-Metriken | 1 |
| `harw-tools::executor` | `TracedToolExecutor` nutzt `harw-observe`-Felder | 1 |
| `harw-plan-bridge::controller` | `reconcile`/`apply` emittieren Schritt-Metriken | 1 |
| `harw-plan-bridge::goal_context` | Coverage-Gauge, `goal_age_days` | 1 |
| `harw-memory` | `context_selector` emittiert scanned/selected | 1 |
| `harw-code-graph` | Konsument: `WorkspaceSensor` (keine Änderung am Crate) | 2 |
| `harw-plan::admission` | zweite Quelle: Dateiereignisse statt nur Worker-Patches | 4 |
| `harw-plan::types` | additive `EvidenceRef`-Erweiterung um optionalen Digest | 4 |
| `harw-knowledge` | `ArtifactKind::{SecurityFinding, Baseline}` | 4 |
| `harw-research` | Konsument: Verdict-Maschinerie nach Vorbild, Scout-Vertrag (keine Änderung am Crate) | 4, 6 |
| `harw-core-bridge` | `ChildReturnContract::SecurityVerdict`, Triage-Fan-out über `fanout_children`, Budgets über `tighten_budget` | 6 |
| `harw-agent-dsl` | Security-Familie, Mixin und Security-Clan (`plan_scope = "security/*"`) als Built-in-Layer | 5 |
| `harw-ops` | `security.rs`: Findings listen, bestätigen, freigeben; `OperationDomain::Security` | 5 |
| `harw-plan-bridge` | `evidence_for_security_finding`, Invalidate/InsertExplore-Vorschlagsquelle | 6 |
| `harw-model-catalog::observed` | Schreiber-Job für Layer 4 | 6 |
| `harw-tui` | Findings-Panel, Freeze-Anzeige, Bestätigungsdialog | 6 |
| `harw-cli` | `harw sentinel`, `harw warden`, `harw security` | 5 |
| `harw-install` | systemd-Units für Sentinel, Warden, Scanner-Timer | 7 |
| `harw-secrets::audit` | periodische Kettenprüfung als Metrik, Off-Host-Spiegel | 7 |
| `harw-home` | Pfade für Reports-Dir, FreezeStore, Ring-Snapshots | 2, 5 |

---

## 9. Wellen

Jede Welle einzeln nützlich, keine hängt an einer späteren. Jede ist als
Zelle formulierbar: `members_from_plan` über die Knoten-IDs der Welle,
`write_partition = Required`, weil die Schreibbereiche crate-disjunkt
geschnitten sind.

**Welle 0: Vokabular.** `harw-observe` und `harw-signals` als Leaves,
`metrics!` und `field!`, neue IDs in `harw-types`, `Redact`-Derive. Keine
Verhaltensänderung, alles Weitere hängt daran.

**Welle 1: Selbstbeobachtung zuerst.** `TraceContext` in `StoredJob` und
`ChildLeaseRecord`, `#[traced]` auf die bestehenden Pfade, Emission der
Metriken, die als Werte längst vorliegen: `ContextAssembly`, `TokenUsage`,
`SelectionResult`, Child-Limits, Lease-Reconciliation, und neu die
Plan-Schleife: Reconcile-Schritte, Goal-Coverage, Scope-Violations. Harwness
sieht sich selbst, bevor es irgendetwas anderes sieht, und die Zahlen helfen
sofort bei der laufenden Planarbeit.

**Welle 2: Host- und Struktur-Observer, rein lesend.** `harw-sensor` mit den
Sysfs- und Procfs-Backends, `AuthSensor` über journald, und dem
`WorkspaceSensor` über `harw-code-graph`. Kein Eingriff, keine Privilegien
über Lesegruppen hinaus. Etabliert beide Ströme und beide Schemata.

**Welle 3: Egress als Menge.** `EgressSet` in `harw-sandbox`,
`harw-netpolicy` als reiner Plan, noch ohne Durchsetzung. Die
Vertragsgrundlage, gegen die später geprüft wird.

**Welle 4: Regelschicht und Verträge.** `harw-rules` mit Vertragsregeln über
`validate_patch` und `contract_from_node`, `FileSensor` mit
loginuid-Attribution, `Baseline`-Artefakte in `harw-knowledge`, die additive
`EvidenceRef`-Erweiterung, die Inventar-Regel für `StructureDrift`. Erste
echte Befunde, Stufe Warnen.

**Welle 5: Durchsetzung.** `harw-warden-proto`, `harw-warden`,
`harw-escalate`, `FreezeStore`, `warden_actions!`. Operator-Operationen in
`harw-ops`, CLI-Subcommands. Ab hier kann das System handeln, aber nur
reversibel und nur bei `ContractViolation`.

**Welle 6: Agentische Triage und Plan-Andockung.** Security-Familie in der
DSL, vier Spezialisierungen, Kontextpolicy, Verdict-Vertrag nach
`harw-research`-Maschinerie, Zellen-Fan-out für Triage,
`evidence_for_security_finding` plus Invalidate/InsertExplore-Vorschläge
durch die Bridge, Injection-Fixtures. Dazu der Layer-4-Schreiber, weil er
dieselbe Auswertungslogik braucht.

**Welle 7: Betrieb und Härtung.** eBPF-Backends für Prozesse und Flows,
systemd-Units in `harw-install`, Off-Host-Spiegelung der Audit-Checkpoints,
Out-of-Band-Meldeweg, periodische Kettenprüfung, Intel-Scout mit
Advisory-Join.

---

## 10. Teststrategie

**Makros:** trybuild-Compile-Fail-Korpus für alle fünf. Insbesondere:
`warden_actions!` ohne Zulässigkeitsregel scheitert, `metrics!` mit
undeklariertem Label scheitert, `#[traced]` auf `async fn` erzeugt
`Instrument` und nicht `enter`.

**Typestate:** je ein Compile-Fail-Fall pro übersprungenem Zustand:
`Finding<Triaged>` aus `Raw`, `Action<Authorized>` außerhalb von
`harw-escalate`, `SensorHandle<Bound>` ohne `bind`.

**Adversarial-Fixtures**, das Gegenstück zu
`authority_elevation_attacker.toml`:

- `injection_logline.json`: Logzeile, deren Inhalt eine Anweisung ist.
- `injection_filename.json`: Dateiname als Prompt-Fragment.
- `injection_process_argv.json`: Prozessname mit Instruktionstext.
- `injection_advisory.json`: Advisory-Text mit eingebetteter Anweisung, muss
  den Review-Gate-Weg nehmen und im Triage-Kontext inert bleiben.
- `injection_fenced_verdict.json`: gültiges Verdikt in Markdown-Fences, muss
  über den toleranten Parser angenommen werden; dasselbe Verdikt mit
  Zusatzfeldern muss an `deny_unknown_fields` scheitern.
- `ceiling_escalation_security.toml`: Security-Definition, die
  `NetworkAccess` hinzuzufügen versucht, muss vom Resolver abgelehnt werden.
- `unauthorized_action.rs`: Versuch, den Warden ohne Proof anzusprechen.

**Zustandsautomaten:** Property-Tests über Freeze- und Lease-Übergänge,
insbesondere Reconciliation nach simuliertem Neustart, analog zu den
bestehenden Child-Lease-Tests.

**Regelschicht:** goldene Fälle pro Regel; die Regeln sind reine Funktionen
mit injiziertem `now` und damit trivial testbar, dieselbe Eigenschaft, die
`PlanController::reconcile` testbar macht.

**Plan-Andockung:** ein Integrationsfall, der einen synthetischen
`ContractViolation`-Befund durch die Bridge laufen lässt und prüft, dass am
Ende genau ein `AttachEvidence` und ein Invalidate-Vorschlag stehen, kein
angewendeter Goal-Übergang, kein zweiter Apply-Pfad.

---

## 11. Entscheidungen: geklärt und offen

### 11.1 Zeitbibliothek, geklärt durch die Realität

Der Workspace hat die Frage praktisch beantwortet: `harw-plan` blieb bei
`time`, und `harw-plan-bridge` ist die dokumentierte Naht, die an genau einer
Stelle konvertiert (`offset_from_timestamp`). Das Konsumfenster mit null
Konsumenten ist geschlossen, `harw-plan` hat jetzt fünf.

Konsequenz für dieses Subsystem: alle acht neuen Crates sind ausnahmslos
`jiff`. Wo Sicherheitsdaten die Plan-Grenze überqueren (Evidenz an
Plan-Knoten), wird ausschließlich der bestehende Nahtpunkt der Bridge
verwendet, niemals eine eigene Konvertierung. Die Regel lautet: pro
Vokabularpaar genau ein benanntes Konvertierungssymbol im gesamten Workspace.
Für Zeit ist das `offset_from_timestamp`, für Konfidenz die eine Abbildung
im Baseline-Kurator (§3.2). Eine spätere `harw-plan`-Migration auf `jiff`
bleibt möglich und wird dadurch sogar einfacher, weil alle Übergänge an einem
Symbol hängen; sie ist aber nicht mehr Voraussetzung dieses Plans.
Empfohlener Wiedervorlagezeitpunkt: jiff 1.0.

### 11.2 `EvidenceRef`, geklärt: nicht inhaltsadressiert

`EvidenceRef` existiert in `harw-plan` mit `kind`, `locator: String`,
`attached_at` (time, rfc3339) und `actor: String`; toleranter
`EvidenceKind`-Fallback auf `Other`. Kein Digest. Für Planarbeit reicht das,
für Sicherheitsnachweise nicht: ein Nachweis, dessen Inhalt nachträglich
austauschbar ist, ist keiner.

Entscheidung: additive Erweiterung statt Verschiebung.
`EvidenceRef` bekommt ein optionales Feld
`digest: Option<ContentDigest>` mit `serde(default)`, bestehende Dateien
bleiben lesbar, die fünf Konsumenten bleiben unberührt.
`evidence_for_security_finding` setzt den Digest immer; der
Plan-seitige Pfad darf ihn weglassen. Ein Verschieben nach `harw-types` wird
nicht mehr empfohlen: `harw-plan` führt seine IDs bewusst lokal, und der
Preis der Verschiebung ist seit der Integration real.

### 11.3 IPC-Transport zum Warden, offen

Unix-Socket mit Peer-Credential-Prüfung (`SO_PEERCRED`) und versioniertem
Rahmen. Empfehlung: systemd-aktivierter Socket, damit der Warden nicht läuft,
wenn niemand ihn braucht.

### 11.4 Kardinalität, offen

`SampleScope::Process` mit `exe_digest` ist potenziell hochkardinal. Für
Prometheus entweder aggregieren oder auf die cgroup-Achse reduzieren. Der
`Cardinality`-Marker ist der Haken, die Politik fehlt.

### 11.5 Multi-Host, bewusst außerhalb

Alles hier ist single-host mit `fs4`-Advisory-Locks. `HostId` ist
vorgesehen, Aggregation über Hosts braucht eine andere Persistenz.

### 11.6 Namensgebung, geklärt

`harw-sentinel` als Binary-Target in `harw-sensor`, `harw-warden` als
eigenes Binary-Crate. Kein DoD-Begriff: ein Department klingt nach
Machtzentrum, und gebaut wird das Gegenteil.

---

## 12. Was dieser Plan bewusst nicht enthält

- Kein eigenes Kernelmodul. Alle Fähigkeiten sind über stabile
  Schnittstellen erreichbar: fanotify, auditd-netlink, Landlock, seccomp,
  eBPF über aya, cgroup v2, netlink. Ein Out-of-Tree-Modul gäbe die
  Versionsstabilität auf, auf der die Langlebigkeitsstrategie beruht.
- Keine eigene Erkennungslogik für Viren oder Rootkits. Etablierte Werkzeuge
  laufen per Timer mit festen Argumenten, der Sentinel liest Reports.
- Kein Ersatz für deterministische Reflexe. Paketfilter, Ratenbegrenzung,
  Integritätsprüfung und seccomp bleiben Kernel und Regelwerk; der Agent
  ersetzt die Urteilsschicht darüber.
- Kein eigener Plan-Zustand. Die Bridge besitzt nichts, das
  Sicherheitssubsystem auch nicht: Findings leben in `harw-knowledge`,
  Plan-Wirkung läuft ausschließlich durch `harw-plan-bridge`.
- Keine Verhaltensbewertung von Personen. Änderungsattribution ja,
  Profilbildung nein, siehe S11.

---

## 13. Änderungen gegenüber v1

1. Basis von 41 auf 46 Member gehoben; die Plan-Integration ist Voraussetzung
   statt Ausblick.
2. Neuer Abschnitt §4.5: Plan-Andockung über `harw-plan-bridge`
   (AttachEvidence, Invalidate-Vorschlag, InsertExplore, terminale Übergänge
   bleiben menschlich, Zellen-Fan-out für Triage).
3. `WorkspaceSensor` über `harw-code-graph`: Struktur-Drift des eigenen
   Workspaces als Ereignisstrom, Supply-Chain-Regel gegen das kuratierte
   Inventar, S6-konformer Advisory-Join über den Intel-Scout.
4. Verdict-Vertrag nach `harw-research`-Maschinerie (Schema plus Prompt plus
   fence-toleranter Parser) statt Eigenbau; Intel-Scout verwendet
   `harw-research` unverändert; `harw_research::Confidence` als
   Verdikt-Konfidenz, eine benannte Abbildung auf das epistemische Vokabular.
5. Zeitfrage neu entschieden: Naht statt Migration, ein Konvertierungssymbol
   pro Vokabularpaar, `offset_from_timestamp` als Präzedenz.
6. `EvidenceRef`-Frage geklärt: additive Digest-Erweiterung in `harw-plan`
   statt Verschiebung nach `harw-types`.
7. Neue Metrikfamilien: Plan-Schleife (Reconcile-Schritte, Goal-Coverage,
   Proposals) und Struktur-Selbstbild; Nullzähler-Liste um
   `goal_status_applied_by_model` erweitert, Ablehnungszähler ausdrücklich
   als Gesundheitssignale statt Brüche eingeordnet.
8. Wellen umgebaut: Welle 1 enthält die Plan-Schleifen-Metriken, Welle 2 den
   Struktur-Observer, Welle 6 die Plan-Andockung; alle Wellen als Zellen
   formulierbar.
9. Actor-Konvention vereinheitlicht mit der bestehenden
   `EvidenceRef`-Konvention (`human:`, `model:`, `warden`).
10. Volle Reconcile-Schrittmenge eingearbeitet: `MarkReady` und `AdmitJobs`
    ersetzen jeden eigenen Security-Scheduler, agentische
    Sicherheitsarbeit fährt als Plan-Knoten auf der Job-Bridge; `AskModel`
    als Präzedenz für gebundene Modellfragen übernommen, ebenso die
    Reihenfolge-Konvention Evidenz vor Invalidierung vor Bereitschaft.
11. Organisationsschicht genutzt: Security-Clan mit `plan_scope =
    "security/*"`, Leader `incident-correlator@1`, Intel-Scout
    organisatorisch außerhalb; S6 zusätzlich als Strukturaussage.
12. Rückgabeweg über `harw-core-bridge::ChildReturnContract`
    (`SecurityVerdict`-Arm) statt Parallelmechanismus; Kind-Budgets über
    `tighten_budget`; Triage-Fan-out über `fanout_children`.
13. Die S-Invarianten werden als Goal-Invarianten des Sicherheits-Goals
    registriert und über den `GoalContextProvider` in jedem Turn
    mitgeführt; sie überleben damit Compaction und Modellwechsel.
