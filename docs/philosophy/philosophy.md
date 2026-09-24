Agent Harnesses als Betriebssystemschicht für langlebige, modellheterogene KI-Agenten

Architektur-Synthese und Einordnung von Harwness

Ausgangspunkt

Die Entwicklung leistungsfähiger KI-Agenten wird häufig fast ausschließlich als Modellproblem betrachtet: Welches Modell plant besser, schreibt besseren Code, nutzt Tools zuverlässiger oder kann mehr Kontext verarbeiten?

Diese Betrachtung ist unvollständig.

Ein Modell erzeugt Entscheidungen, Vorschläge und strukturierte Handlungsabsichten. Es besitzt jedoch weder Prozesse noch Dateien, Netzwerkverbindungen, Jobs, Child Agents, Berechtigungen oder dauerhaften Zustand. All diese Dinge existieren nur durch die Software, die das Modell umgibt.

Die zentrale These lautet deshalb:

«Ein Modell besitzt agentisches Potenzial. Erst das Harness verwandelt dieses Potenzial in kontrollierte, langlebige und überprüfbare Arbeit.»

Die praktische Agentenleistung lässt sich eher als multiplikatives System verstehen:

Effektive Agentenleistung
=
Modellfähigkeit
× Schnittstellenqualität
× Kontextqualität
× Runtime-Verlässlichkeit
× Verifikation

Ein sehr leistungsfähiges Modell in einem schwachen Harness kann schlechter arbeiten als ein etwas schwächeres Modell in einer gut angepassten Runtime. Wenn Lifecycle-Management, Tool-Schnittstellen oder Kontextaufbau versagen, nähert sich das Gesamtergebnis unabhängig vom Modell der Null.

Diese Synthese basiert auf drei Quellenklassen:

1. einer statischen Analyse des aktuellen Harwness-Workspace mit 26 Rust-Crates und rund 53.000 Zeilen Rust,
2. der Entstehungsgeschichte aus Audits von "codex-rs", Hermes und OpenClaw,
3. aktuellen Primärquellen zu Agentenarchitekturen, Kontextmanagement, MCP, Durable Execution und unterschiedlichen Modellfähigkeiten.

Der hier untersuchte Export befindet sich weiterhin in aktiver Entwicklung. Ein eigener vollständiger Workspace-Build konnte in der Analyseumgebung mangels Rust-Toolchain nicht erneut ausgeführt werden. Aussagen über implementierte Architektur beziehen sich daher auf den tatsächlich vorhandenen Code, nicht auf eine behauptete aktuelle Gesamtverifikation.

---

1. Modell, Agent, SDK und Harness sind nicht dasselbe

Ein Sprachmodell ist zunächst ein probabilistisches Inferenzsystem. Es erhält Kontext und erzeugt Ausgaben.

Ein Agent ergänzt dieses Modell um einen Regelkreis:

Kontext aufnehmen
→ Entscheidung treffen
→ Werkzeug oder Aktion auswählen
→ Umweltergebnis beobachten
→ Zustand aktualisieren
→ fortfahren oder beenden

Anthropic beschreibt Agents entsprechend als Systeme, in denen das Modell seine Prozesse und Tool-Nutzung dynamisch steuert. Gleichzeitig wird betont, dass Agents Ground Truth aus ihrer Umgebung, klare Stop-Bedingungen, Sandboxing und sorgfältig entworfene Tool-Schnittstellen benötigen.

Ein Agents SDK stellt Entwicklern Abstraktionen bereit, um solche Regelkreise in Software einzubauen. Das OpenAI Agents SDK liefert beispielsweise einen Agent Loop, Function Tools, Agents-as-Tools, Handoffs, Sessions, Guardrails und Tracing.

Ein Harness geht darüber hinaus. Es muss Agenten nicht nur ausdrücken, sondern dauerhaft betreiben:

- Prozesse starten und beenden,
- Modelle und Provider normalisieren,
- Tool Calls ausführen,
- Berechtigungen begrenzen,
- Sessions und Jobs persistieren,
- Child Agents beaufsichtigen,
- Abstürze überstehen,
- Approvals verwalten,
- Kontext zusammenstellen,
- Ergebnisse verifizieren,
- Clients und Remote Channels bedienen,
- verwaiste oder überholte Arbeit beseitigen.

Ein vollständiges persönliches Agentensystem besitzt daher mindestens vier Ebenen:

Client Plane
TUI, CLI, Web, Telegram, mobile Clients

Control Plane
Gateway, Admission, Scheduler, Jobs, Policies, Agent Registry

Execution Plane
Modelle, Tools, MCP, Sandbox, Prozesse, Child Agents

State Plane
Sessions, Transcripts, Jobs, Memory, Artifacts, Audit, Approvals

Harwness entwickelt sich genau in diese Richtung. Es ist weder nur eine Agents SDK noch nur eine Coding-CLI. Es wird zu einer lokalen Agenten-Control-Plane mit eigener Runtime.

---

2. Warum Modellvielfalt eine Runtime-Frage ist

Aktuelle Modelle werden mit deutlich unterschiedlichen Fähigkeiten und Betriebscharakteristika angeboten.

GLM-5.2 wird ausdrücklich als Modell für Long-Horizon-Aufgaben mit einem stabilen Kontextfenster von einer Million Tokens und variabler Reasoning-Intensität positioniert.

Kimi K2 wird als agentisches Modell mit starker Tool-Calling-Fähigkeit beschrieben. Seine offizielle Dokumentation zeigt zugleich, dass der umgebende Client die Tool-Schemas bei jedem Aufruf bereitstellen, Tool Calls korrekt parsen, die Funktionen ausführen und die Ergebnisse wieder in den Modellverlauf einfügen muss.

Grok 4.5 wird aktuell mit agentischem Tool Calling, konfigurierbarem Reasoning und einem Kontextfenster von 500.000 Tokens angeboten.

Diese Angaben beschreiben Fähigkeiten, aber noch kein zuverlässiges Laufzeitverhalten. Zwei Modelle mit nominell gleichem Kontextfenster und gleichem Function-Calling-Protokoll können sich erheblich darin unterscheiden:

- wie strikt sie JSON-Schemas einhalten,
- wann sie Tools statt Freitext wählen,
- wie sie Toolfehler interpretieren,
- ob sie parallele Aktionen sinnvoll strukturieren,
- wie schnell sie bei langen Verläufen den Fokus verlieren,
- wie gut sie nach einer Kompaktierung weiterarbeiten,
- ob sie Delegation sparsam oder inflationär einsetzen,
- welche Promptstruktur sie für stabile Arbeit benötigen,
- wie zuverlässig sie einen Task als beendet erkennen.

Deshalb reicht eine Provider-Abstraktion wie diese nicht aus:

async fn respond(messages, tools) -> ModelResponse

Ein leistungsfähiges Harness benötigt zwei getrennte Modelleigenschaften.

Deklarierte Provider-Fähigkeiten

Diese beschreiben die technische API:

struct ModelCapabilities {
    context_limit: usize,
    supports_tools: bool,
    supports_parallel_tools: bool,
    supports_streaming: bool,
    supports_reasoning_effort: bool,
    supports_prompt_caching: bool,
    supports_images: bool,
}

Gemessenes Laufzeitprofil

Dieses entsteht aus Evaluationen und realen Runs:

struct ModelBehaviorProfile {
    tool_call_reliability: Score,
    long_context_retention: Score,
    delegation_discipline: Score,
    schema_strictness: Score,
    compaction_tolerance: Score,
    retry_sensitivity: Score,
    preferred_task_granularity: TaskGranularity,
    safe_parallelism: usize,
}

Das erste Profil sagt, was die API grundsätzlich erlaubt. Das zweite sagt, wie das konkrete Modell unter einem bestimmten Harness tatsächlich arbeitet.

Ein Modell-Router sollte daher nicht bloß nach Providername oder Benchmarkrang entscheiden, sondern nach Rolle:

Orchestrator
stark in Zerlegung, Konfliktauflösung und Synthese

Repository Worker
stark bei großem Kontext und längerer Akkumulation

Focused Coding Worker
stark bei eng abgegrenzten Implementierungen

Verifier
konservativ, schemafest und prüfungsorientiert

Scout
günstig, schnell, read-only und hoch parallelisierbar

Modellheterogenität ist damit keine Störung, die vollständig hinter einem einheitlichen Interface verschwinden sollte. Sie ist eine Ressource, die das Harness gezielt ausnutzen muss.

---

3. Warum große Kontextfenster keine Memory-Architektur ersetzen

Ein sehr großes Kontextfenster bedeutet nicht, dass möglichst viele Daten hineingegeben werden sollten.

Anthropic beschreibt Kontext als endliche Ressource mit abnehmendem Grenznutzen. Mit wachsendem Kontext können Fokus, Retrieval-Präzision und die Verarbeitung weit auseinanderliegender Abhängigkeiten nachlassen. Effektives Context Engineering sucht daher nicht die größtmögliche, sondern die kleinste hochsignifikante Tokenmenge für die aktuelle Entscheidung.

Das bedeutet:

«Kontext muss konstruiert werden, nicht einfach akkumulieren.»

Eine brauchbare Agenten-Runtime sollte mindestens zwischen folgenden Informationsarten unterscheiden:

Working Context
Daten für den unmittelbar aktuellen Turn

Session History
Kommunikations- und Entscheidungsverlauf

Job State
dauerhafter Stand der aktuellen Arbeit

Workbench
strukturierte menschlich sichtbare Arbeitsartefakte

Diary
zeitlich orientierte Erfahrungs- und Aktivitätsaufzeichnung

Memory
kuratierte, längerfristig relevante Erkenntnisse

Skills
wiederverwendbare prozedurale Fähigkeiten

Artifacts
Dateien, Reports, Patches und andere konkrete Ergebnisse

Hermes verbindet heute persistenten User-Kontext, Memory, Skills und MCP mit einem expliziten Self-Improvement-Ansatz.  OpenClaw setzt stark auf einen dauerhaften Gateway-Dienst, Channels, Skills, Sandboxing und optionale Clients.

Harwness übernimmt solche Produktideen nicht als einen einzigen unstrukturierten Prompt-Ordner. Die aktuelle Crate-Struktur trennt in "harw-knowledge" unter anderem:

- Memory Core,
- Recall und Topics,
- Memory Palace,
- Diary,
- Dream,
- Workbench,
- Kanban,
- Artifacts,
- Visibility.

Diese Trennung ist bedeutsam. Ein Memory-Eintrag ist nicht automatisch ein Skill. Ein Diary-Eintrag ist nicht automatisch dauerhaftes Wissen. Ein Workbench-Artefakt ist nicht automatisch Teil des Modellkontexts.

Jede Informationsklasse braucht eigene Promotion-, Retention- und Visibility-Regeln.

---

4. Memory-Konsolidierung ist ein langlebiger Workflow

Der beschriebene zweiphasige Memory-Plan zeigt besonders deutlich, wie Harwness denkt.

Phase 1

Viele Rollouts können parallel ausgewertet werden. Sie produzieren normalisierte, rollout-lokale Memory-Datensätze.

Phase 2

Die globalen Memory-Artefakte werden serialisiert aktualisiert. Nur ein Consolidation-Prozess darf gleichzeitig den gemeinsamen Memories-Workspace untersuchen und verändern.

Das ist keine bloße „LLM fasst Erinnerungen zusammen“-Funktion, sondern ein langlebiges Konsistenzprotokoll:

Stage-1-Daten auswählen
→ Workspace deterministisch synchronisieren
→ Git-Diff gegen letzten Erfolg bilden
→ bei leerem Diff ohne Agent beenden
→ isolierten Consolidation-Agent starten
→ Lease währenddessen heartbeaten
→ Ergebnis prüfen
→ Git-Baseline aktualisieren
→ DB-Erfolg und Watermark persistieren

Besonders stark ist die Trennung:

Watermark
= Buchhaltung über bekannte Eingangsdaten

Workspace-Dirtiness
= tatsächliche Entscheidung, ob Konsolidierungsarbeit nötig ist

Ein Watermark darf nicht als alleiniger Dirty Check dienen. Der Datenbankstand kann aktuell aussehen, während die Dateiartefakte unvollständig oder inkonsistent sind. Der reale Git-Diff bildet dagegen Änderungen, Löschungen und vom Agenten erzeugte Ausgaben gemeinsam ab.

Git wird damit nicht nur Versionsverwaltung, sondern:

- Baseline,
- Change Detector,
- Audit-Layer,
- begrenzter Promptkontext,
- nachvollziehbarer Commit-Punkt.

Der Workflow ähnelt Durable-Execution-Systemen: Temporal beschreibt durable Ausführung als Fähigkeit, Anwendungen nach Prozess-, Netzwerk- oder Infrastrukturfehlern an der vorherigen Stelle fortzusetzen.

Für eine endgültig robuste Phase 2 sollte der persistente Zustand die entscheidenden Commit-Grenzen ausdrücken:

Claimed
→ WorkspaceSynced
→ AgentCompleted
→ BaselineCommitted
→ DatabaseCommitted

Dabei müssen alle Schritte idempotent oder sicher rekonstruierbar sein. Der Agent darf nicht die Konsistenz garantieren; er arbeitet innerhalb eines von der Runtime garantierten Protokolls.

---

5. Ein Goal ist Desired State, kein langer Prompt

Der gesetzte Goal-Mechanismus spielt dabei eine zentrale Rolle.

Ein Goal ist nicht bloß eine Formulierung, die am Anfang einer Chat-Session erscheint. Es beschreibt den dauerhaft gewünschten Endzustand. Der Plan ist dagegen nur die gegenwärtige Strategie, diesen Zustand zu erreichen.

Goal
Was muss am Ende wahr sein?

Plan
Welche Strategie verfolgen wir momentan?

Tasks und Jobs
Welche konkreten Arbeitseinheiten werden ausgeführt?

Verification
Welche Evidenz beweist die Zielerreichung?

Das entspricht dem Controller-Prinzip aus verteilten Systemen: Ein Controller beobachtet den aktuellen Zustand und versucht fortlaufend, ihn dem gewünschten Zustand anzunähern.

Für Coding-Agenten bedeutet dies:

- Ein Context-Compact darf das Goal nicht zerstören.
- Ein Modellwechsel darf die Zielsemantik nicht verändern.
- Ein fehlgeschlagener Worker darf den Plan ändern, aber nicht stillschweigend das Goal reduzieren.
- Neue Erkenntnisse dürfen den Plan revidieren.
- „Fertig“ wird durch Acceptance Criteria und Evidenz bestimmt, nicht durch das Gefühl des Modells.

Ein persistiertes Goal könnte daher ungefähr enthalten:

struct Goal {
    id: GoalId,
    statement: String,
    invariants: Vec<Invariant>,
    acceptance_criteria: Vec<Criterion>,
    constraints: Vec<Constraint>,
    status: GoalStatus,
    plan_version: u64,
    evidence: Vec<EvidenceRef>,
}

Die von Mia genutzte Formulierung „continue working … get inspired by … and regard: [architecture review]“ funktioniert bereits als Steuerungshierarchie:

Goal
weiterarbeiten und das gewünschte System fertigstellen

Sources of Inspiration
OpenClaw-, Hermes- und Memory-/Self-Improvement-Muster

Architectural Regard
Invarianten, Failure Windows und Grenzen des Reviews

Autonomie
Orchestrator darf den konkreten Plan eigenständig anpassen

Das verhindert das typische Agentenverhalten, nach jeder Analyse wieder um Erlaubnis zur Umsetzung zu bitten.

---

6. Das Modell darf Arbeit vorschlagen, aber niemals Prozesse besitzen

Viele Multi-Agent-Systeme behandeln einen Spawn sinngemäß so:

Modell sagt „starte Agent“
→ neuer Agent wird gestartet

Das ist zu schwach.

Ein Modelloutput ist untrusted Intent. Daraus darf erst nach serverseitiger Admission eine reale Arbeitseinheit entstehen:

Delegation Intent
→ Schema- und Policy-Prüfung
→ Deduplizierung
→ Budgetzuweisung
→ Authority-Reduktion
→ Child Job anlegen
→ Lease vergeben
→ Worker starten
→ Heartbeats überwachen
→ Ergebnis persistieren
→ Parent Join
→ Cleanup

Das Modell besitzt also weder den Child Agent noch dessen Prozess. Es erhält lediglich einen Handle auf kontrollierte Arbeit.

Diese Trennung ist entscheidend, weil natürlichsprachliches Prompting keine fehlende Infrastruktur ersetzen kann. Ein Modell kann angewiesen werden, sparsam zu delegieren. Es kann jedoch keinen fehlenden Process Reaper, kein Fencing und keine persistente Parent-Child-Beziehung herbeiprompten.

---

7. Multi-Agent ist ein langlebiger Ausführungsgraph

Ein produktionsfähiges Multi-Agent-System ist nicht einfach:

await gather(run_agent(a), run_agent(b), run_agent(c))

Es ist ein dauerhafter, überprüfbarer Ausführungsgraph:

Parent Job
├── Turn
├── Tool Calls
├── Child Job A
│   ├── Authority Snapshot
│   ├── Budget
│   ├── Lease
│   ├── Heartbeat
│   └── Result
├── Child Job B
└── Completion Barrier

Jeder Child benötigt mindestens:

- eine eindeutige Identität,
- Parent- oder Promotion-Semantik,
- ein idempotentes Spawn-Key,
- ein festes Budget,
- reduzierte Capabilities,
- einen Lease-Owner,
- eine Cancellation-Policy,
- Join- und Result-Semantik,
- einen terminalen Zustand,
- Reaping oder Tombstone-Handling.

Die aktuelle Harwness-Codebasis besitzt dafür bereits bemerkenswert viele konkrete Mechanismen:

- "LeaseToken { work_id, epoch, nonce }",
- langlebige Jobs,
- Fencing,
- Cancellation,
- Retry- und Budgetinformationen,
- Managed Child Controller,
- Child Leasing,
- Reaping,
- Tombstones,
- Capability Snapshots,
- parallele Child Runs,
- Actor-gebundene Single-Use-Approvals.

Der besonders wichtige Ablauf lautet:

Durable Cancellation und Fencing persistieren
→ Store-Lock verlassen
→ alten Worker signalisieren

Nicht umgekehrt.

Wenn zunächst nur ein Prozesssignal gesendet wird und die Persistenz anschließend scheitert, kann der Zustand nicht eindeutig rekonstruiert werden. Wenn dagegen zuerst Epoch beziehungsweise Fencing-Identität aktualisiert werden, kann ein alter Worker nach Reclaim oder Cancellation weder renewen noch erfolgreich completen.

Leases und Heartbeats sind etablierte Koordinationsmechanismen. Kubernetes verwendet Lease-Objekte und aktualisierte "renewTime"-Zeitstempel, um die Verfügbarkeit von Teilnehmern zu bestimmen.

Für Agents folgt daraus:

«Kein Child ohne Lease, kein Lease ohne Fencing, kein terminaler Zustand ohne persistierte Completion.»

So verhindert die Runtime, dass aus Multi-Agent-Orchestrierung ein Haufen doppelt laufender, verwaister oder semantisch überholter „Zombie-Agenten“ wird.

---

8. Recovery muss durch Reconciliation erfolgen

Ein bloßer Neustart des Prozesses ist keine Recovery-Strategie.

Nach einem Gateway-Neustart muss die Runtime abgleichen:

Was behauptet der persistente Store?
Welche Worker gelten als claimed?
Welche Leases sind abgelaufen?
Welche Prozesse oder Tasks existieren tatsächlich?
Welche Children haben einen lebenden Parent?
Welche Jobs wurden durable gecancelt?
Welche Ergebnisse wurden bereits committed?

Daraus folgt ein Reconciliation Loop:

Beobachten
→ Abweichung feststellen
→ zulässige Korrektur bestimmen
→ Aktion ausführen
→ neuen Zustand persistieren
→ erneut beobachten

Dieser Controller-Ansatz ist für eine Agenten-Runtime besser geeignet als ein rein imperatives Modell von „starten, warten, fertig“.

Er erlaubt:

- abgelaufene Jobs neu zu claimen,
- überholte Worker zu fencen,
- fehlende Children erneut zu starten,
- bereits persistierte Ergebnisse nicht doppelt zu committen,
- verwaiste Jobs nach Policy zu canceln oder zu promoten,
- den Gateway-Prozess selbst austauschbar zu machen.

---

9. Die Provisioning-Schicht ist der eigentliche Agenten-Compiler

Tools, Skills, MCP, Profile und Agent Definitions sollten nicht einfach in einen großen Prompt oder eine globale Toolliste geschoben werden.

Eine Agenteninstanz sollte aus mehreren Quellen deterministisch kompiliert werden:

User Profile
+ Agent Definition
+ Goal und Job Context
+ aktive Skills
+ verfügbare Operations
+ MCP-Kataloge
+ Channel Restrictions
+ Sandbox Policy
+ Approval Policy
+ Provider-Fähigkeiten
────────────────────────
Resolved Agent Capability View

Diese Capability View sollte als Snapshot an einen Turn oder Job gebunden werden. Dadurch kann sich die globale Umgebung später ändern, ohne dass die Bedeutung einer bereits laufenden Arbeit stillschweigend wechselt.

Die wichtigsten Begriffe müssen getrennt bleiben:

Profile beschreiben Nutzer-, Umgebungs- und Provider-Defaults.

Agent Definitions beschreiben Rolle, Instruktionen, Modelle, Skills, Tools, Budgets und Authority.

Skills enthalten prozedurales Wissen und gegebenenfalls Ressourcen.

Operations sind semantische Aktionen, die über verschiedene Surfaces exponiert werden können.

MCP ist ein Protokoll, über das Tools, Ressourcen und Prompts bereitgestellt werden.

Session ist Kommunikations- und Arbeitskontext.

Job ist die langlebige ausführbare Arbeitseinheit.

Workbench ist die menschlich sichtbare Projektion von Arbeit und Ergebnissen.

Kanban ist eine organisatorische Ansicht auf Workbench und Jobs.

Das MCP-Protokoll stellt selbst keine vollständige Security Boundary dar. Seine Spezifikation weist ausdrücklich darauf hin, dass MCP beliebige Datenzugriffe und Codeausführung ermöglichen kann und dass Autorisierung, Consent und Access Control von der implementierenden Anwendung bereitgestellt werden müssen.

Harwness behandelt deshalb MCP-Daten nicht als Autorität. Der vorhandene Code bindet Sessions an serverseitig aufgelöste Principals, filtert "tools/list" nach Capabilities und soll Scope, Budget, Sandbox, Tenant und Credentials ausschließlich aus vertrauenswürdiger Composition beziehen.

Das ist die richtige Richtung:

«Ein MCP-Server kann Möglichkeiten beschreiben. Er darf sich keine Rechte selbst verleihen.»

---

10. Operations sind semantische Aktionen, nicht bloß Tools oder Commands

Die neue "harw-operations"-Schicht ist einer der wichtigsten Teile der aktuellen Architektur.

Eine Operation wird einmal semantisch definiert und kann anschließend gezielt auf verschiedenen Oberflächen verfügbar werden:

Command Surface
direkte Nutzeraktion, etwa /status oder /model

Model Tool Surface
vom Modell aufrufbare strukturierte Funktion

Agent Tool Surface
gekapselter Child-Agent oder spezialisierter Workflow

Channel Surface
remote verfügbare, eventuell weiter reduzierte Aktion

Die vorhandenen Typen unterscheiden bereits:

- "CommandVisibility",
- "ApprovalPolicy",
- "PermissionTier",
- "Surface",
- "OpInvocation::Command",
- "OpInvocation::ModelTool",
- "OpInvocation::AgentTool".

Das ist bedeutend, weil "/quit", "/model" oder "/attach" nicht automatisch als Modell-Tool verfügbar sein sollten. Eine gemeinsame Quelle bedeutet nicht, dass jede Oberfläche dieselben Rechte besitzt.

Eine Operation kann beispielsweise deklarieren:

#[operation(
    name = "status",
    command = "/status",
    model_tool,
    permission = "observer",
    approval = "none"
)]
async fn status(
    ctx: &OpContext,
    args: StatusArgs,
) -> Result<StatusOutput, StatusError> {
    // ...
}

Das Proc Macro generiert daraus mechanische Infrastruktur:

- Metadaten,
- Argument-Schema,
- Adapter,
- Registry-Eintrag,
- Dispatch,
- Tool-Spezifikation,
- Compile-Time-Validierung.

Das Macro ist damit kein bloßer Boilerplate-Generator. Es wird zu einem kleinen Compiler für Systeminvarianten.

Es kann beispielsweise erzwingen:

- dass mutierende Model Tools eine Approval Policy besitzen,
- dass Command-Pfade eindeutig sind,
- dass Argumente surface-spezifisch geparst werden,
- dass eine Agent Operation Budget und Authority-Reducer definiert,
- dass nicht jede Operation auf jedem Channel sichtbar ist.

Diese hohe Implementierungsdichte passt zu Mias allgemeinem Rust-Stil: starke Typen und Typestate im Fundament, darüber eine kompakte modulare DSL.

---

11. Skills und Tools sind eine Agent-Computer-Schnittstelle

Tool-Design wird oft als einfache API-Integration behandelt. Für Modelle ist es jedoch eine eigene Benutzeroberfläche.

Anthropic berichtet, dass bei einem Coding-Agenten mehr Zeit in die Optimierung der Tools als in den Gesamtprompt investiert wurde. Parameter, Pfadsemantik, Beschreibungen und Fehlerformen beeinflussen direkt, ob ein Modell ein Werkzeug zuverlässig verwenden kann.

Eine gute Tool-Schnittstelle muss deshalb:

- klar abgegrenzt sein,
- eindeutige Parameter besitzen,
- strukturierte Fehler liefern,
- keine unnötige Formatierungsarbeit verlangen,
- keine Authority im Input akzeptieren,
- idempotente Wiederholung ermöglichen,
- relevante Resultate begrenzen,
- bei Mutation Verifikation oder Approval verlangen.

Ein Harness sollte Tools außerdem modellabhängig anbieten können. Ein Modell, das bei vielen ähnlichen Tools häufig verwechselt, kann ein reduziertes Toolset erhalten. Ein anderes kann progressive Tool Discovery oder hierarchische Kataloge sinnvoll nutzen.

Skills ergänzen diese Schnittstelle um prozedurales Wissen:

Tool
Was kann technisch ausgeführt werden?

Skill
Wie, wann und unter welchen Bedingungen sollte eine Fähigkeit eingesetzt werden?

Operation
Welche semantische Aktion wird angeboten?

Policy
Wer darf diese Aktion über welche Surface ausführen?

Diese Trennung ist wesentlich sauberer als ein gigantischer Systemprompt, in dem Fähigkeiten, Regeln, Tutorials und Berechtigungen miteinander vermischt werden.

---

12. Authority muss monoton reduziert werden

Eine der wichtigsten Harwness-Invarianten lautet sinngemäß:

«Untrusted Input darf Rechte niemals erweitern.»

Daraus folgt eine monotone Authority-Pipeline:

System Maximum
∩ Principal Capabilities
∩ Agent Definition
∩ Job Scope
∩ Channel Restrictions
∩ Child-Agent Reduction
∩ Sandbox Policy
=
Effective Authority

Jeder weitere Schritt darf Rechte nur erhalten oder reduzieren, niemals erhöhen.

Dies gilt besonders für:

- Telegram- oder andere Remote Channels,
- MCP-Clients,
- Skills,
- Modell-Tool-Argumente,
- Child Agents,
- wiederaufgenommene Jobs.

Ein vom Modell erzeugter Tool Call darf daher niemals selbst bestimmen:

- Tenant,
- Workspace,
- Work ID,
- Worker ID,
- Budget,
- Retry Policy,
- Sandbox,
- Credentials,
- Permission Tier.

Diese Werte müssen serverseitig aus Principal, Session, Agent Definition, Job und Policy aufgelöst werden.

Der aktuelle Harwness-Handoff formuliert genau solche Grenzen: Remote- und MCP-Daten sind keine Autorität, Job Scope ist immutable, Lease Tokens werden nicht über MCP serialisiert und Principal-Auflösung geschieht serverseitig.

Das ist einer der Punkte, an denen Harwness über viele heutige Agent Frameworks hinausdenkt. Es modelliert nicht nur, was ein Agent tun kann, sondern auch, woher die Erlaubnis dafür stammt.

---

13. Observability ist Teil des Protokolls

Tracing darf nicht erst nachträglich um den Agent Loop gelegt werden. Es muss die semantischen Zustandsübergänge des Systems abbilden.

Eine sinnvolle Span-Hierarchie ist:

agent.session
└── agent.turn
    ├── context.assemble
    ├── model.request
    ├── model.response
    ├── tool.request
    │   └── tool.execute
    ├── approval.wait
    ├── child.spawn
    │   └── child.turn
    ├── artifact.persist
    └── verification

Die aktuelle Harwness-Codebasis emittiert bereits unter anderem:

- Turn-Spans,
- Tool-Request-Events,
- Tool-Completion-Events,
- Usage-Aggregation,
- Turn-Failure-Events,
- Approval- und Handoff-Zustände.

Die TUI konsumiert inzwischen Operation-Adapter sowie Session- und Turn-Events. Der aktuelle Export erstellt im TUI-Pfad allerdings weiterhin eine lokale "AgentSession" und einen "InMemoryStateStore". Auch der Gateway besitzt noch lokale In-Memory-Sessions. Die beabsichtigte Architektur eines vollständig dünnen Clients ist damit klar erkennbar, im analysierten Stand aber noch nicht vollständig durchgezogen.

Langfristig sollte allein der Gateway die Runtime besitzen:

Gateway
besitzt Sessions, Jobs, Provider, Tools, Memory und Events

TUI
sendet Intents und rendert Event Streams

Remote Channel
sendet reduzierte Intents und erhält reduzierte Ergebnisse

Dann kann die TUI beendet, neu gestartet oder durch einen anderen Client ersetzt werden, ohne dass Agentenarbeit stirbt.

---

14. Die wichtigsten Fehlerklassen heutiger Harnesses

Interface Mismatch

Der Provider unterstützt Tools, aber der Adapter verliert Tool Calls, Tool-Result-IDs, Reasoning-Parameter oder Usage-Daten.

Context Pollution

Der Harness hängt ungefiltert immer mehr History, Tools, Skills und Dateien an jeden Turn.

Lifecycle Leakage

Children besitzen keine eindeutigen Parents, Leases, Deadlines oder terminalen Zustände.

Duplicate Work

Retries und Restarts erzeugen neue Arbeit, obwohl der ursprüngliche Job noch existiert oder bereits abgeschlossen wurde.

Authority Leakage

Tool- oder Channel-Inputs dürfen Scope, Workspace, Credentials oder Sandbox beeinflussen.

Zombie Accumulation

Veraltete Worker bleiben aktiv, weil Fencing, Reaping oder Reconciliation fehlen.

Non-Durable Progress

Ein langer Agentenlauf existiert nur im RAM und verschwindet beim Prozessabbruch.

Provider Over-Normalization

Unterschiedliche Modelle werden auf das kleinste gemeinsame Interface reduziert, wodurch besondere Fähigkeiten nicht genutzt werden.

Prompt-Based Governance

Sicherheits- und Lifecycle-Regeln werden dem Modell erklärt, statt von der Runtime erzwungen zu werden.

Completion Without Evidence

Das Modell erklärt die Aufgabe für erledigt, ohne Tests, Diff, Artifact oder andere objektive Nachweise.

Diese Fehler verstärken sich gegenseitig. Ein verlorener Tool Call kann einen Retry auslösen; der Retry erzeugt einen zweiten Child; beide schreiben in denselben Workspace; der Parent kompaktifiziert; nach dem Restart kennt die Runtime keinen der beiden Worker mehr.

Das Ergebnis wirkt wie ein „dummes Modell“, ist aber in Wirklichkeit ein fehlendes Betriebssystem.

---

15. Referenzablauf eines langlebigen Coding-Goals

Ein vollständiger Ablauf sollte eher so aussehen:

1. User formuliert Goal
2. Runtime persistiert Desired State und Acceptance Criteria
3. Orchestrator erzeugt versionierten Plan
4. Plan wird in einen Job Graph übersetzt
5. Admission prüft Budget, Authority und Parallelität
6. Model Router wählt passende Modelle pro Rolle
7. Worker erhalten begrenzte Capability Snapshots
8. Tools und Child Jobs laufen unter Leases und Fencing
9. Events, Traces und Artifacts werden durable geschrieben
10. Verifier prüfen Tests, Diff und Zielkriterien
11. Controller reconciled Abweichungen oder Fehler
12. Erfolgreiche Ergebnisse werden committed
13. Memory Pipeline extrahiert und konsolidiert relevante Erkenntnisse
14. Goal erhält Evidenz und terminalen Status

Der Orchestrator darf seine Strategie ändern. Das Goal und dessen Invarianten bleiben stabil.

---

16. Die zentralen Designprinzipien

Aus Forschung, Codebasis und Systems Engineering ergeben sich folgende Invarianten:

1. Das Modell formuliert Intent; die Runtime besitzt Authority und Lifecycle.

2. Jeder langlebige Effekt wird vor oder atomar mit seiner Ausführung persistiert.

3. Fencing geschieht vor dem Signalisieren eines überholten Workers.

4. Jeder Spawn besitzt Parent-, Promotion-, Budget-, Lease- und Cleanup-Semantik.

5. Capabilities dürfen entlang der gesamten Pipeline nur reduziert werden.

6. Provider-Adapter müssen semantisch verlustfrei statt nur syntaktisch einheitlich sein.

7. Kontext wird pro Turn konstruiert, nicht unbegrenzt angesammelt.

8. Memory ist eine Promotion-Pipeline, kein unkontrolliertes Langzeit-Transcript.

9. Clients besitzen keine Runtime.

10. Recovery erfolgt durch Reconciliation gegen durable State.

11. Observability bildet semantische Zustände ab und ist kein optionales Logging.

12. Eine Aufgabe ist erst fertig, wenn überprüfbare Evidenz die Acceptance Criteria erfüllt.

13. Multi-Agent-Parallelität ist ein Budget, kein Selbstzweck.

14. Tools, Skills, Operations und Policies bleiben getrennte Konzepte.

15. Proc Macros dürfen Boilerplate verbergen, aber keine unprüfbare Authority erzeugen.

---

17. Einordnung von Harwness

Harwness ist am treffendsten nicht als „eigener Codex“ und auch nicht nur als Agent Framework zu beschreiben.

Es ist eine entstehende:

«lokale, modellheterogene Agent Operating Environment mit Control Plane, langlebiger Job Runtime, Capability-Provisioning, Wissenssystem und austauschbaren Clients.»

Die Inspirationslinien sind klar:

- "codex-rs" lieferte Beispiele dafür, wie eine ernsthafte Coding-Agent-Runtime, Sandbox und Terminalintegration in Rust umgesetzt werden können.
- Hermes inspirierte Teile der Provider-, Skill-, Memory- und persönlichen Agentenebene.
- OpenClaw inspirierte Gateway-, Channel-, Diary-, Dreaming- und dauerhaft betriebene Agentenfunktionen.
- Die eigene Agents SDK, Operation-Syntax, Jobs-Ebene, Authority-Logik, MCP-/Skill-Provisioning und Systemzusammensetzung bilden jedoch ein eigenständiges Modell.

Die originäre Leistung liegt nicht darin, jedes einzelne Feature erfunden zu haben. Sie liegt in der Synthese und in den Grenzen zwischen den Komponenten:

Session ist nicht Job.
Job ist nicht Agent.
Agent ist nicht Modell.
Skill ist nicht Tool.
Tool ist nicht Permission.
Workbench ist nicht Memory.
Client ist nicht Runtime.
MCP ist nicht Authority.
Goal ist nicht Plan.

Genau diese Trennung ermöglicht später Erweiterbarkeit, ohne dass das System zu einer Sammlung impliziter Seiteneffekte zerfällt.

---

Schlussfolgerung

Die nächste Generation persönlicher KI-Agenten wird nicht allein durch stärkere Modelle entstehen.

Modelle werden zunehmend größere Kontexte, besseres Tool Calling, längere Reasoning-Horizonte und stärkere Coding-Fähigkeiten besitzen. Diese Fortschritte vergrößern jedoch zugleich die Anforderungen an ihre Betriebsumgebung.

Je länger und autonomer ein Agent arbeitet, desto wichtiger werden:

- langlebiger Zustand,
- kontrollierte Delegation,
- Recovery,
- Context Engineering,
- Authority Boundaries,
- modellabhängige Runtime-Profile,
- Verifikation,
- transparente Clients,
- deterministische Memory-Pipelines.

Die entscheidende technische Frage lautet deshalb nicht nur:

«Welches Modell ist am intelligentesten?»

Sondern:

«Welche Runtime kann die Stärken unterschiedlicher Modelle zuverlässig in dauerhafte, kontrollierte und überprüfbare Arbeit übersetzen?»

Harwness adressiert genau diese zweite Frage.

Der langfristige Wert des Projekts liegt damit nicht lediglich in einer besseren TUI oder einer weiteren Tool-Calling-Abstraktion. Er liegt in einer vollständigen Agenten-Betriebsschicht, die Modelle als austauschbare Entscheidungsorgane behandelt und alle realen Ressourcen — Prozesse, Rechte, Arbeit, Wissen und Lebenszyklen — selbst kontrolliert.

Ein Modell kann planen, delegieren und programmieren.

Aber erst ein gutes Harness sorgt dafür, dass daraus kein Zombiehaufen, sondern ein belastbares System wird.
