//! Fassade des Verteidigungssubsystems (DoD) — der einzige Name, den der
//! Rest des Workspaces vom DoD-Teilbaum kennt (Knoten **AW6-09**).
//!
//! # Warum dieser Knoten existiert
//! Der DoD-Teilbaum umfasst 27 Crates: vier Zugriffs-/Datenvokabular-
//! Grundlagen (`harw-dod-cap`, `harw-dod-signals`, `harw-dod-readfs`,
//! `harw-dod-fixtures`), dreizehn Crates, die `trait Sensor` implementieren
//! (`-cpu`, `-thermal`, `-memory`, `-blockio`, `-netcounters`, `-gpu`,
//! `-cgroup`, `-listener`, `-scanreport`, `-workspace`, `-authlog`,
//! `-fsmon`, `-procmon`), zwei geteilte Infrastruktur-Crates, die selbst
//! keinen `Sensor` sind (`harw-dod-bpf`: Ladeschicht für `-procmon` und
//! `-flow`; `harw-dod-netlink`: Transport für `-authlog`), `harw-dod-flow`
//! (Übersetzung roher Bpf-Ereignisse in `SecurityEvent`, ohne eigenen
//! `Sensor`-Typ), einen Bewertungspfad (`harw-dod-rules`,
//! `harw-dod-sentinel`) und eine Eskalations-/Durchsetzungskette
//! (`harw-dod-escalate`, `harw-dod-warden-proto`, `harw-dod-warden`,
//! `harw-dod-netpolicy`). Ohne Fassade müsste ein Konsument, der nur „den
//! Host beobachten" will, ein Dutzend Pfadabhängigkeiten aufnehmen und
//! wissen, welche davon zueinander gehören. Diese Crate war seit fünf
//! Architektur-Abschnitten benannt, aber nie gebaut (K13) — dieser Knoten
//! schließt das.
//!
//! # Das Auswahlkriterium (Vorbild: `harw-lens`)
//! **Eine Fassade wählt aus, sie reicht nicht alles weiter.** Das Kriterium
//! für „gehört zur Fassadenfläche" ist nicht „ist `pub` in einer
//! Innencrate", sondern (siehe `harw-lens`s `//!`-Block für die
//! ausführliche Fassung):
//!
//! 1. Ist es Parameter- oder Rückgabetyp eines re-exportierten Typs/einer
//!    re-exportierten Funktion selbst?
//! 2. Ist es ein Werkzeug, ohne das ein Aufrufer diese Parameter nicht
//!    sinnvoll befüllen könnte, ohne dabei selbst physische Innenteile zu
//!    berühren?
//! 3. Würde das Zurückhalten dieses Namens eine Entscheidung verschweigen,
//!    die eigentlich dem Aufrufer gehört?
//!
//! Kein `pub use *` — jeder Name unten ist einzeln benannt.
//!
//! # Die drei Verben, die ein Konsument braucht — und eines, das er hier
//! nicht bekommt
//! Ein Konsument dieser Fassade will „beobachte den Host" und „bewerte
//! einen Befund". Er bekommt **nicht** „setze eine Aktion durch" — das ist
//! der wichtigste Entwurfsentscheid dieses Knotens, siehe Abschnitt
//! „Was ausdrücklich nicht dazugehört" unten.
//!
//! ## „Beobachte den Host"
//! - **Zugriffsvokabular** ([`harw_dod_cap`]): [`Capability`],
//!   [`CapabilityClass`], [`ReadScope`], [`SensorHandle`], [`Bound`],
//!   [`Unbound`], [`SensorError`], [`Permanence`] — ohne das kann kein
//!   Aufrufer einen Griff bauen (`SensorHandle::new(id, capability).bind(scope)`),
//!   den jeder Sensor unten als `From<SensorHandle<Bound>>`-Konstruktion
//!   verlangt.
//! - **Datenvokabular** ([`harw_dod_signals`]): [`Sensor`] (der eine Trait,
//!   den jeder Sensor implementiert), [`SensorReading`], [`SecurityEvent`],
//!   [`EventKind`], [`Actor`], [`AuthOutcome`], [`DriftSeverity`],
//!   [`HostSample`], [`Severity`], [`Hardness`], [`SecurityEvidence`] —
//!   Rückgabe- bzw. Feldtypen von [`Sensor::poll`]/[`Sensor::handle`] und
//!   von [`RuleContext`] unten.
//!   **Nein** — `SecurityVerdict`, `SuggestedResponse`,
//!   `VerdictClassification`, `parse_and_validate_verdict`,
//!   `parse_verdict`, `validate_verdict` (alle in
//!   `harw_dod_signals::verdict`): zum Zeitpunkt dieses Knotens hat keine
//!   einzige Crate im Workspace einen Aufrufer dieser vier Funktionen
//!   (geprüft per Volltextsuche) — Fläche für ein Vokabular freizugeben,
//!   das noch nirgends verdrahtet ist, wäre verfrüht, kein vergessener
//!   Fall.
//! - **Der Aggregator** ([`harw_dod_sentinel`]): [`Sentinel`],
//!   [`SentinelConfig`], [`SentinelError`], [`SentinelResult`],
//!   [`SensorHealth`], [`DegradeReason`], [`RetryPolicy`],
//!   [`EvidenceBuffer`] — der empfohlene Einstiegspunkt: eine feste
//!   Sensorenliste abrufen, Gesundheitszustand je Sensor führen, Belege
//!   puffern, statt dass jeder Konsument dieselbe Rückversuchs-/
//!   Degradierungslogik neu schreibt.
//! - **Die neun unprivilegierten Instrumente** — konkret, weil ohne sie das
//!   Zugriffsvokabular oben nichts zu binden hätte: [`CpuSensor`],
//!   [`ThermalSensor`], [`MemorySensor`], [`BlockioSensor`],
//!   [`NetCountersSensor`], [`GpuSensor`], [`ListenerSensor`],
//!   [`ScanReportSensor`], [`REPORT_HARDNESS`], [`WorkspaceDriftSensor`].
//!   Ausgewählt über `xtask/src/gate_privileges.rs`s `CRATE_PRIVILEGE`-
//!   Tabelle: neun der zehn dort als `RequiredPrivilege::Unprivileged`
//!   geführten Sensor-Crates (die zehnte ist `harw-dod-cgroup`, siehe unten).
//!   **Nein** — `harw_dod_workspace::{Inventory, Edge, StructureChange,
//!   VersionSeverity, classify_version_change, WorkspaceError,
//!   WorkspaceResult}`: internes Analysevokabular, mit dem
//!   `WorkspaceDriftSensor` seine eigene `SensorReading` baut;
//!   `WorkspaceDriftSensor::new`s dritter Parameter
//!   (`baseline: Option<Inventory>`) lässt sich mit `None` befüllen, ohne
//!   `Inventory` je zu benennen — wie `harw-lens` bei `ByteSpan` bleibt
//!   dieser Typ innerhalb der einen Crate, die ihn braucht.
//!   **Nein — `harw-dod-cgroup`**: Gerüst aus Knoten AW0-00, Inhalt entsteht
//!   erst in Knoten AW2-13. Es gibt noch keinen gelandeten Namen, den diese
//!   Fassade auswählen oder verschweigen könnte — eine Lücke, kein
//!   vergessener Fall (siehe „Bekannte Lücke" unten).
//!
//! ## „Bewerte einen Befund" ([`harw_dod_rules`])
//! [`run_rules`], [`Rule`], [`RuleContext`], die drei mitgelieferten Regeln
//! ([`EgressFlowRule`], [`BaselineDeviationRule`], [`StructureDriftRule`]),
//! [`Finding`], [`FindingKind`], [`RuleChecked`], [`Triaged`], [`Verdict`],
//! [`triage`], [`Baseline`], [`PalaceStatus`] — alles direkte Parameter-,
//! Rückgabe- oder Feldtypen von `run_rules`/`triage`/`RuleContext`.
//!
//! **Nein** — `harw_dod_rules::Raw` (der dritte Zustandsmarker): kein
//! re-exportierter Name dieser Fassade gibt oder nimmt je ein
//! `Finding<Raw>` entgegen — `run_rules` liefert bereits `Finding<RuleChecked>`
//! (`Finding::raw`/`Finding::check` sind in `harw-dod-rules` `pub(crate)`,
//! siehe dessen `finding.rs`-Moduldoku). Ihn zu benennen böte keinem
//! Aufrufer dieser Fassade etwas.
//!
//! **Nein** — `finding_kind_for_status`: ausschließlich ein internes
//! Werkzeug von `BaselineDeviationRule::evaluate` (siehe
//! `harw-dod-rules/src/rules/baseline_deviation.rs`); ein Aufrufer, der
//! `run_rules` mit dieser Regel aufruft, bekommt das Ergebnis bereits als
//! fertigen `Finding<RuleChecked>` zurück und ruft diese Funktion nie
//! selbst auf.
//!
//! **Nein** — `epistemic_confidence_for`: von keiner Regel in
//! `harw-dod-rules` selbst aufgerufen (geprüft per Volltextsuche) — eine
//! eigenständige Konvertierung von `harw-research`s Konfidenzskala, die an
//! keinem re-exportierten Typ/keiner re-exportierten Funktion dieser
//! Fassade hängt. Ein Aufrufer, der sie braucht, hat `harw-research`
//! (den Quelltyp der Konvertierung) ohnehin bereits vorliegen.
//!
//! # Was ausdrücklich nicht dazugehört: die Eskalations-/
//! Durchsetzungskette
//! `harw-dod-escalate`, `harw-dod-warden-proto`, `harw-dod-warden` und
//! `harw-dod-netpolicy` sind **nicht** Teil dieser Fassade — auch nicht
//! hinter einem Feature. Vier Gründe:
//!
//! 1. **Invariante S1.** `harw_dod_escalate::Action::authorize` ist in
//!    `harw-dod-escalate` `pub(crate)` — die einzige Stelle im gesamten
//!    Workspace, die eine `AuthorizationProof` erzeugt (siehe dessen
//!    `action.rs`-Moduldoku samt eigenem `compile_fail`-Doctest). Das bleibt
//!    unabhängig davon wahr, ob diese Fassade `Action`/`Authorized`
//!    re-exportiert — `pub(crate)` überlebt jeden `pub use`, auch einen
//!    Blanket-Reexport, weil ein nicht-`pub`-Item über `pub use krate::*`
//!    gar nicht erst sichtbar wird. Diese Fassade re-exportiert dennoch
//!    **keinen einzigen Namen** aus `harw-dod-escalate` — nicht weil ein
//!    Reexport `authorize` erreichbar machen würde (das kann er nicht),
//!    sondern weil `harw-dod-escalate`s öffentliche Funktionen
//!    (`freeze_ops::authorize_freeze`/`authorize_release`/
//!    `authorize_stage_gated`) bereits fertige `Action<Authorized>`-Werte
//!    zurückgeben, die zum Warden weitergereicht werden sollen — sie unter
//!    „beobachte/bewerte" zu präsentieren verwischte genau die Grenze,
//!    die dieser Abschnitt zieht. Der eigene `compile_fail`-Doctest unten
//!    belegt das für diese Fassade: es gibt hier **gar keinen** Pfad zu
//!    `Action`, `Authorized` oder `Action<Authorized>` — stärker als
//!    „unerreichbar", nämlich „nicht einmal benennbar".
//! 2. **Eine zweite Abhängigkeit würde durchgereicht.**
//!    `freeze_ops::authorize_freeze`/`authorize_release`/
//!    `authorize_stage_gated`/`reconcile_expired_freezes` nehmen alle ein
//!    `&harw_session_store::FreezeStore` entgegen — eine vierte Crate, die
//!    diese Fassade dann ebenfalls in ihre eigene Fläche hätte aufnehmen
//!    müssen, nur um `harw-dod-escalate` sinnvoll benutzbar zu machen.
//! 3. **`harw-dod-warden` ist der Durchsetzer, kein allgemeines Werkzeug.**
//!    Er wird von genau einem Aufrufer gebraucht — dem `harw-warden`-Binary,
//!    das `harw-dod-warden`/`harw-dod-warden-proto` bereits direkt in
//!    seiner eigenen `Cargo.toml` nennt. Ein Konsument, der nur messen
//!    will, hätte mit dem Durchsetzer in seiner Abhängigkeitshülle nichts
//!    gewonnen und ein Stück Angriffsfläche mehr — genau die Frage, die
//!    dieser Knoten laut Auftrag stellen sollte, beantwortet mit „nein,
//!    auch nicht optional".
//! 4. **`harw-dod-netpolicy` ist kein Sensor.** Es implementiert `Sensor`
//!    nicht und liefert keine `SensorReading` — es plant `NetPlan`-Regeln
//!    über `harw-sandbox`s bereits eigenständigem `NetworkScope`/
//!    `EgressTarget`-Vokabular, als Vorbereitung für `harw-dod-warden`s
//!    `NetworkIsolator`. Es passt weder zu „beobachte" noch zu „bewerte";
//!    es gehört, wenn überhaupt, zu einer künftigen, eigens begründeten
//!    Durchsetzungs-Fassade — nicht stillschweigend hierher.
//!
//! # Die Privilegienfrage
//! Drei Fähigkeitsklassen tauchen in `xtask/src/gate_privileges.rs`s
//! `CRATE_PRIVILEGE`-Tabelle auf: `harw-dod-authlog`
//! (`RequiredPrivilege::Netlink`), `harw-dod-fsmon`
//! (`RequiredPrivilege::FileWatch`) und `harw-dod-bpf`/`harw-dod-procmon`/
//! `harw-dod-flow` (`RequiredPrivilege::Bpf`). Reexportierte sie diese
//! Fassade unbedingt, erbte **jeder** Konsument, der nur `harw-dod` in
//! seine `Cargo.toml` schreibt, alle drei Klassen in seine
//! Abhängigkeitshülle — und `gate_privileges.rs` sähe das, weil es die
//! *tatsächliche* Cargo-Abhängigkeitskante prüft, nicht ob ein Feature sie
//! aktiviert hat.
//!
//! Deshalb liegen die fünf Fähigkeits-Crates hinter dem `privileged`-
//! Feature dieser Crate, als `optional = true`-Pfadabhängigkeiten. Ohne das
//! Feature ist `AuthlogSensor`, `FsMonSensor`, `ProcmonSensor`,
//! `observe` (aus `harw-dod-flow`) und die `BpfLoader`-Familie schlicht
//! nicht vorhanden — kein `#[cfg]`-verstecktes totes Feld, sondern eine
//! Abhängigkeitskante, die ohne das Feature gar nicht existiert.
//!
//! **Wogegen das schützt:** ein Konsument, der `harw-dod` ohne weitere
//! Angabe in seine `Cargo.toml` schreibt, bekommt keine der drei
//! Privilegienklassen — weder im Reexport noch als Cargo-Abhängigkeitskante
//! (siehe `tests/facade.rs`, das genau das gegen diese `Cargo.toml` prüft,
//! nach dem Vorbild von `harw-dod-flow`s eigenem
//! `test_cargo_toml_declares_no_aya_dependency`).
//!
//! **Wogegen das *nicht* schützt (K53/AW5-01):** Cargo-Features sind
//! additiv und werden über den gesamten Abhängigkeitsgraphen **einer**
//! Kompilierungseinheit vereinigt. Aktiviert irgendeine andere Crate im
//! Abhängigkeitsbaum *desselben Binaries* `harw-dod/privileged`, bekommt
//! dieses Binary die fünf Fähigkeits-Crates ebenfalls — unabhängig davon,
//! ob das Binary selbst das Feature je genannt hat. Dieses Feature ist
//! also eine Zusicherung über den **Reexport dieser einen Fassade an einen
//! Konsumenten, der sie unverändert übernimmt**, keine
//! Sicherheitsgrenze zwischen Binaries. Die tatsächliche Grenze — „welche
//! Fähigkeitsklasse darf in der *Hülle eines bestimmten Binaries*
//! auftauchen" — prüft ausschließlich `xtask/src/gate_privileges.rs`, das
//! jedes der vier Binaries einzeln gegen sein deklariertes Budget
//! (`BinaryBudget`) prüft, unabhängig von jedem Feature-Flag. Ein Binary,
//! das eine erhöhte Fähigkeit nicht tragen darf, muss deshalb `harw-dod`
//! entweder ohne `privileged` einbinden oder — besser — die betroffene
//! Sensor-Crate (z. B. `harw-dod-cpu`) direkt statt über diese Fassade
//! nennen: dieses Gate ist die Kontrolle, nicht das Feature.
//!
//! # `Action<Authorized>` bleibt unerreichbar
//! Siehe den `compile_fail`-Doctest am Ende dieses Blocks: kein Pfad dieser
//! Fassade führt zu `Action`, `Authorized` oder `Action<Authorized>` — die
//! Namen existieren in dieser Crate schlicht nicht (siehe „Was ausdrücklich
//! nicht dazugehört" oben). Das entwertet AW5-03s eigenen
//! `compile_fail`-Doctest in `harw-dod-escalate::action` nicht: jener
//! belegt, dass `authorize` innerhalb von `harw-dod-escalate` selbst nicht
//! von außen aufrufbar ist; dieser hier belegt zusätzlich, dass **diese
//! Fassade** den Typ, um den es dabei geht, nicht einmal benennt.
//!
//! ```rust,compile_fail
//! // `Action` und `Authorized` existieren in `harw_dod` nicht — dieser
//! // Import schlägt bereits bei der Namensauflösung fehl (E0432), nicht
//! // erst bei einem Zugriffsversuch.
//! use harw_dod::{Action, Authorized};
//!
//! fn attempt(a: Action<Authorized>) {
//!     let _ = a;
//! }
//! ```
//!
//! # Bekannte Lücke
//! `harw-dod-cgroup` ist zum Zeitpunkt dieses Knotens noch ein leeres
//! Gerüst (kein `pub`-Item außer der Moduldoku). Sobald AW2-13 landet, ist
//! zu prüfen, ob `CgroupSensor` (oder wie der konkrete Typ dann heißt) den
//! Kriterien oben genügt — voraussichtlich ja, analog zu den neun bereits
//! aufgenommenen Sensoren. Das ist bewusst nicht Teil dieses Knotens.
//!
//! # Nebenläufigkeit
//! Alle Typen dieser Fassade sind entweder reine Daten oder — wie
//! [`Sentinel`] — bereits in ihrer eigenen Crate für Mehrfadenbetrieb
//! entworfen (`Sentinel::poll_all` hält `Arc<dyn Sensor>`; `Sensor:
//! Send + Sync` ist Teil des Vertrags in `harw-dod-signals`). Diese Fassade
//! selbst hält keinen eigenen Zustand und führt keine eigene Logik aus.
//!
//! # Fehler
//! Kein eigener Fehlertyp — jede re-exportierte Funktion liefert den
//! Fehlertyp ihrer Ursprungscrate unverändert zurück
//! ([`SensorError`], [`SentinelError`]/[`SentinelResult`], mit
//! `privileged`-Feature zusätzlich `AuthlogError`/`AuthlogResult`,
//! `FsMonError`/`FsMonResult`, `BpfError`,
//! `ProcmonError`/`ProcmonResult`, `FlowError`/`FlowResult`). Eine
//! eigene Verdichtung wie bei `harw-lens`s `LensError` wäre hier verfrüht:
//! anders als bei `build`/`ask` definiert diese Fassade keine eigenen
//! Funktionen, deren Fehlerfläche verdichtet werden müsste.
//!
//! # Examples
//! „Beobachte den Host" — ein Griff, ein Sensor, ein Abruf:
//!
//! ```rust
//! use harw_dod::{Capability, CpuSensor, ReadScope, Sensor, SensorHandle};
//! use harw_types::SensorId;
//! use std::path::PathBuf;
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
//! let handle = SensorHandle::new(SensorId::from_str("cpu-0"), Capability::ReadProcStat)
//!     .bind(scope);
//! let sensor = CpuSensor::from(handle);
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH);
//! assert!(reading.is_ok());
//! ```
//!
//! „Bewerte einen Befund" — eine Regel gegen ein beobachtetes Ereignis
//! auswerten und triagieren:
//!
//! ```rust
//! use harw_dod::{Actor, EgressFlowRule, EventKind, Rule, RuleContext, SecurityEvent, Verdict, run_rules, triage};
//! use harw_authority::NetworkScope;
//! use harw_types::SensorId;
//!
//! let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
//! let events = vec![SecurityEvent {
//!     sensor: SensorId::from_str("net-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None::<Actor>,
//!     kind: EventKind::EgressFlow {
//!         destination: "evil.example.com".to_owned(),
//!         port: 443,
//!     },
//! }];
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//! let rule: &dyn Rule = &EgressFlowRule;
//! let checked = run_rules(&[rule], &ctx);
//! let finding = checked.into_iter().next().expect("EgressFlowRule loest aus");
//! let triaged = triage(finding, Verdict::Confirmed);
//! assert_eq!(*triaged.verdict(), Verdict::Confirmed);
//! ```
//!
//! # Stand
//! Knoten **AW6-09**; Ebene **L5** im Zielgraphen. Abhängigkeiten:
//! `harw-dod-cap`, `harw-dod-signals`, `harw-dod-sentinel`,
//! `harw-dod-rules`, die neun unprivilegierten Sensor-Crates (alle
//! vorgelagert gelandet), optional (`privileged`-Feature) `harw-dod-authlog`,
//! `harw-dod-fsmon`, `harw-dod-bpf`, `harw-dod-procmon`, `harw-dod-flow`.

// --- harw-dod-cap: Zugriffsvokabular (siehe Begründung oben) ---
pub use harw_dod_cap::{
    Bound, Capability, CapabilityClass, Permanence, ReadScope, SensorError, SensorHandle, Unbound,
};

// --- harw-dod-signals: Datenvokabular (siehe Begründung oben) ---
pub use harw_dod_signals::{
    Actor, AuthOutcome, DriftSeverity, EventKind, Hardness, HostSample, SecurityEvent,
    SecurityEvidence, Sensor, SensorReading, Severity,
};

// --- harw-dod-sentinel: der Aggregator (siehe Begründung oben) ---
pub use harw_dod_sentinel::{
    DegradeReason, EvidenceBuffer, RetryPolicy, SensorHealth, Sentinel, SentinelConfig,
    SentinelError, SentinelResult,
};

// --- harw-dod-rules: "bewerte einen Befund" (siehe Begründung oben) ---
pub use harw_dod_rules::{
    Baseline, BaselineDeviationRule, EgressFlowRule, Finding, FindingKind, PalaceStatus, Rule,
    RuleChecked, RuleContext, StructureDriftRule, Triaged, Verdict, run_rules, triage,
};

// --- die neun unprivilegierten Instrumente (siehe Begründung oben) ---
pub use harw_dod_blockio::BlockioSensor;
pub use harw_dod_cpu::CpuSensor;
pub use harw_dod_gpu::GpuSensor;
pub use harw_dod_listener::ListenerSensor;
pub use harw_dod_memory::MemorySensor;
pub use harw_dod_netcounters::NetCountersSensor;
pub use harw_dod_scanreport::{REPORT_HARDNESS, ScanReportSensor};
pub use harw_dod_thermal::ThermalSensor;
pub use harw_dod_workspace::WorkspaceDriftSensor;

// --- `privileged`-Feature: die drei Fähigkeitsklassen (siehe "Die
// Privilegienfrage" oben). Ohne das Feature existieren diese Namen nicht. ---

/// Netlink-Fähigkeit (`RequiredPrivilege::Netlink`): Anmeldeereignisse.
#[cfg(feature = "privileged")]
pub use harw_dod_authlog::{
    AuditBackend, AuthBackend, AuthRecord, AuthlogError, AuthlogResult, AuthlogSensor,
    FixtureAuthBackend,
};

/// FileWatch-Fähigkeit (`RequiredPrivilege::FileWatch`): Dateisystemereignisse.
#[cfg(feature = "privileged")]
pub use harw_dod_fsmon::{
    FixtureFsEventSource, FsEventSource, FsMonError, FsMonResult, FsMonSensor, RawFsEvent,
    shape_event,
};

/// Bpf-Fähigkeit (`RequiredPrivilege::Bpf`): Ladeschicht, geteilt von
/// `harw-dod-procmon` und `harw-dod-flow`.
#[cfg(feature = "privileged")]
pub use harw_dod_bpf::{
    BpfError, BpfHandle, BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec,
    FixtureBpfLoader, RawBpfEvent,
};

/// Bpf-Fähigkeit: Prozessausführung.
#[cfg(feature = "privileged")]
pub use harw_dod_procmon::{
    DEFAULT_READ_TIMEOUT, ExecEvent, ProcmonError, ProcmonResult, ProcmonSensor, parse_exec_payload,
};

/// Bpf-Fähigkeit: Egress-Fluss. `observe`/`to_security_event` verlangen ein
/// `&harw_authority::NetworkScope` — diese Fassade re-exportiert
/// `NetworkScope` nicht (siehe `harw-dod-rules`s Begründung oben, Abschnitt
/// „Bewerte einen Befund"); ein Konsument dieser Funktionen hat
/// `harw-sandbox` ohnehin bereits vorliegen.
#[cfg(feature = "privileged")]
pub use harw_dod_flow::{
    Direction, FlowError, FlowEvent, FlowResult, Protocol, observe, parse_flow_payload,
    to_security_event,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
