# Wave 4 — Agents as Tools

**Spec-Quelle**: harwness-Stack Design-Docs, Wave 4  
**Status**: Entwurf  
**Datum**: 2026-07-15  
**Scope**: `AgentToolAdapter` — Child-Agents als aufrufbare Tools im Parent-LLM-Loop

---

## Inhaltsverzeichnis

1. [Motivation](#1-motivation)
2. [Grundinvariante — Authority-Monotonie](#2-grundinvariante--authority-monotonie)
3. [Semantik: Handoff vs. Agent-as-Tool](#3-semantik-handoff-vs-agent-as-tool)
4. [AgentToolAdapter — Skizze](#4-agenttooladapter--skizze)
5. [Budget & Cancellation](#5-budget--cancellation)
6. [Transcript-Einbettung](#6-transcript-einbettung)
7. [Parent/Child-Fencing bei Crash/Resume](#7-parentchild-fencing-bei-crashresume)
8. [Registrierung via `#[operation]`-Makro](#8-registrierung-via-operation-makro)
9. [Nicht in Welle 4](#9-nicht-in-welle-4)
10. [Testing-Plan](#10-testing-plan)

---

## 1. Motivation

Klassische Agenten-Handoffs (`transfer_to_agent`) sind ein Vollübergabe-Muster: der aufrufende Agent gibt Kontrolle ab, der neue Agent übernimmt den gesamten Loop. Das ist semantisch einfach, aber für viele Aufgaben zu grob — besonders wenn der Parent weiter orchestrieren muss, nachdem eine spezialisierte Sub-Aufgabe erledigt ist.

**Agents as Tools** kehrt die Kontrolle um: das Modell des Parent-Agents ruft einen Child-Agent wie ein gewöhnliches Tool auf, wartet auf das strukturierte Ergebnis und fährt mit dem eigenen Loop fort. Der Child-Agent ist dabei vollständig gekapselt — sein interner Verlauf, seine Tool-Calls und seine Zwischenschritte sind dem Parent nicht direkt sichtbar (es sei denn, `include_transcript` ist aktiviert, siehe Abschnitt 6).

### Referenz: OpenAI Agents SDK

Das OpenAI Agents SDK exponiert dieses Muster über `agent.as_tool()`:

```python
researcher = Agent(name="researcher", instructions="...")
tool = researcher.as_tool(
    tool_name="research_topic",
    tool_description="Recherchiert ein Thema und gibt eine Zusammenfassung zurück.",
)
```

Der harwness-Stack implementiert ein äquivalentes Muster auf Rust-Ebene über den `AgentToolAdapter`, der den bestehenden `Operation`-Contract nutzt und nahtlos mit `ManagedAgentSpawner` und `AwaitingChild` zusammenarbeitet.

### Warum jetzt (Welle 4)?

- `ManagedAgentSpawner` (Welle 2) liefert sicheres Child-Lifecycle-Management.
- `AwaitingChild` (Welle 3) löst das Synchronisierungsproblem für Parent-Loops.
- Parallele Tool-Ausführung ist bereits implementiert — ein Child-Agent ist strukturell nur ein weiteres Tool, das parallelisiert werden kann.
- Die fehlende Klammer war das Authority-Reduktions-Modell, das in Welle 4 eingeführt wird.

---

## 2. Grundinvariante — Authority-Monotonie

> **Ein Child-Agent besitzt niemals mehr Authority als der Parent-Call, der ihn erzeugt.**

Diese Invariante ist die sicherheitskritische Grundlage des gesamten Adapters. Sie ist nicht optional und kann zur Laufzeit nicht überschrieben werden.

### Konkrete Ausprägungen

| Dimension | Regel |
|---|---|
| **Sandbox-Permissions** | `child_permissions ⊆ parent_permissions` — strekte Teilmenge |
| **Capabilities** | Ein Child kann keine Capability aktivieren, die der Parent nicht hat |
| **Approval-Modus** | `child_approval_level ≤ parent_approval_level` (Owner > Operator > User > Observer) |
| **Tool-Whitelist** | Der Child bekommt nur Tools, die der Parent explizit delegiert |
| **Network-Access** | Child-Netzwerk-Scope ist eine Teilmenge des Parent-Scopes |

### Beispiele

```
Owner-Parent  → kann Observer-Child spawnen     ✓ (Monotonie)
Owner-Parent  → kann Operator-Child spawnen     ✓
Observer-Parent → kann Owner-Child spawnen      ✗ (Authority-Verletzung, Laufzeitfehler)
Operator-Parent → kann Operator-Child spawnen   ✓ (gleiche Ebene ist erlaubt)
```

Die `authority_reducer`-Funktion (Abschnitt 4) ist per Signatur monoton fallend: sie nimmt eine `SandboxSpec` und gibt eine `SandboxSpec` zurück, die ausschließlich Rechte entfernt, niemals hinzufügt. Eine Implementierung, die Rechte hinzufügt, wird beim ersten `invoke` mit `OpError::AuthorityViolation` abgewiesen.

### Verifizierung zur Laufzeit

Der `AgentToolAdapter::invoke` prüft vor dem Spawn:

1. `child_sandbox.permissions ⊆ ctx.sandbox().permissions` — mengentheoretisch
2. `child_sandbox.approval_level ≤ ctx.sandbox().approval_level` — ordinale Ordnung
3. Falls eine der Bedingungen verletzt ist: sofortiger `OpError::AuthorityViolation`, kein Spawn

---

## 3. Semantik: Handoff vs. Agent-as-Tool

Diese Gegenüberstellung beschreibt die zwei zentralen Delegations-Muster im harwness-Stack:

### Handoff (`transfer_to_agent`)

- Der aktuelle Agent beendet seinen Loop.
- Die Konversation wird vollständig an den neuen Agent übergeben.
- Der ursprüngliche Agent hat danach keine Kontrolle mehr.
- Geeignet für: Weiterleitungen, Eskalationen, spezialisierte Endpunkte.
- Analogie: Telefonweiterleitung — der ursprüngliche Mitarbeiter legt auf.

### Agent-as-Tool

- Der Parent-Agent ruft den Child-Agent wie ein Tool auf.
- Der Parent-Loop läuft weiter, nachdem der Child geantwortet hat.
- Der Child-Agent ist vollständig gekapselt (Black Box).
- Das Ergebnis des Child ist ein strukturierter `OpOutput`.
- Geeignet für: Recherche-Subtasks, spezialisierte Berechnungen, Code-Generierung, Zusammenfassungen — alles, was ein definiertes Ende hat.
- Analogie: Funktionsaufruf — der Aufrufer bekommt einen Rückgabewert.

### Wann welches Muster?

| Kriterium | Handoff | Agent-as-Tool |
|---|---|---|
| Parent muss nach der Delegation weiterarbeiten | Nein | Ja |
| Child-Ergebnis fließt in Parent-Entscheidung | Nein | Ja |
| Konversations-Kontinuität beim Parent | Nein | Ja |
| Budget-Kontrolle für Child notwendig | Schwierig | Ja (nativ) |
| Mehrere Childs parallel möglich | Nein | Ja |

---

## 4. `AgentToolAdapter` — Skizze

Der `AgentToolAdapter` implementiert den `Operation`-Contract und integriert sich damit nahtlos in den bestehenden Tool-Dispatcher des harwness-Stacks.

```rust
/// Adapter, der einen Child-Agent als Parent-seitig aufrufbares Tool exponiert.
/// Implementiert den `Operation`-Contract (harwness Design: interaction-contract.md).
pub struct AgentToolAdapter {
    /// Definition des zu spawnenden Child-Agents.
    child_agent: Arc<AgentDefinition>,

    /// Monoton fallende Funktion: reduziert die Parent-SandboxSpec auf eine
    /// Teilmenge für den Child. DARF NIEMALS Rechte hinzufügen.
    authority_reducer: fn(&SandboxSpec) -> SandboxSpec,

    /// Budget-Limits für den Child-Spawn (Tokens, Tool-Calls, Wandzeit).
    budget: AgentBudget,

    /// Ob der Child-Transcript in den Parent-Context inlined wird.
    include_transcript: bool,
}

impl AgentToolAdapter {
    /// Erstellt einen neuen Adapter. Validiert sofort, dass `authority_reducer`
    /// eine monotone Funktion ist (Smoke-Test mit Maximal-Sandbox).
    pub fn new(
        child_agent: Arc<AgentDefinition>,
        authority_reducer: fn(&SandboxSpec) -> SandboxSpec,
        budget: AgentBudget,
        include_transcript: bool,
    ) -> Result<Self, OpError> {
        // Monotonie-Smoke-Test: Maximal-Sandbox rein, Ergebnis muss Teilmenge sein.
        let max_sandbox = SandboxSpec::maximum();
        let reduced = authority_reducer(&max_sandbox);
        if !reduced.is_subset_of(&max_sandbox) {
            return Err(OpError::AuthorityViolation(
                "authority_reducer fügt Rechte hinzu — verboten".into(),
            ));
        }
        Ok(Self { child_agent, authority_reducer, budget, include_transcript })
    }

    /// Ruft den Child-Agent auf und wartet auf sein Ergebnis.
    ///
    /// Ablauf:
    ///   1. Authority reduzieren (monoton verifiziert).
    ///   2. Child-OpContext bauen (session_id vererben, turn_id neu).
    ///   3. Budget an Parent-Deadline anpassen (Timeout-Propagation).
    ///   4. Spawn via ManagedAgentSpawner mit Budget-Enforcement.
    ///   5. Auf AwaitingChild warten; Budget-Überschreitung → OpError::Execution.
    ///   6. Transcript optional einbetten.
    ///   7. Strukturiertes OpOutput zurückgeben.
    pub async fn invoke(
        &self,
        ctx: &OpContext,
        input: OpInput,
    ) -> Result<OpOutput, OpError> {
        // 1) Authority reduzieren und verifizieren
        let child_sandbox = (self.authority_reducer)(ctx.sandbox());
        self.verify_authority_monotone(ctx.sandbox(), &child_sandbox)?;

        // 2) Child-OpContext bauen
        let child_ctx = OpContext::builder()
            .session_id(ctx.session_id())       // vererbt
            .turn_id(TurnId::new_child())        // frisch
            .sandbox(child_sandbox)
            .cancellation_token(ctx.cancellation_token().child())
            .build();

        // 3) Budget an Parent-Deadline anpassen
        let effective_budget = self.budget.clamp_to_deadline(ctx.deadline());

        // 4) Spawn via ManagedAgentSpawner
        let spawner = ctx.managed_spawner();
        let child_handle = spawner.spawn(
            Arc::clone(&self.child_agent),
            child_ctx,
            input,
            effective_budget,
        ).await?;

        // 5) Warten — AwaitingChild pollt Cancellation
        let child_result = child_handle.await_result().await?;

        // 6) Transcript optional einbetten
        let output = if self.include_transcript {
            child_result.into_output_with_transcript()
        } else {
            child_result.into_output()
        };

        Ok(output)
    }

    fn verify_authority_monotone(
        &self,
        parent: &SandboxSpec,
        child: &SandboxSpec,
    ) -> Result<(), OpError> {
        if !child.is_subset_of(parent) {
            return Err(OpError::AuthorityViolation(format!(
                "Child-Sandbox {:?} ist keine Teilmenge von Parent-Sandbox {:?}",
                child, parent
            )));
        }
        Ok(())
    }
}
```

### `AgentBudget`-Struktur

```rust
/// Budget-Limits für einen Child-Agent-Spawn.
pub struct AgentBudget {
    /// Maximale Token-Anzahl (Input + Output) über alle Turns des Childs.
    pub max_tokens: u32,

    /// Maximale Anzahl an Tool-Calls des Childs.
    pub max_tool_calls: u32,

    /// Maximale Wandzeit in Millisekunden.
    pub max_wall_time_ms: u64,
}

impl AgentBudget {
    /// Kürzt das Budget so, dass der Child vor der Parent-Deadline fertig ist.
    /// Gibt das kleinere der beiden Zeitlimits zurück.
    pub fn clamp_to_deadline(&self, parent_deadline: Option<Instant>) -> Self {
        match parent_deadline {
            None => self.clone(),
            Some(deadline) => {
                let remaining_ms = deadline
                    .duration_since(Instant::now())
                    .as_millis() as u64;
                Self {
                    max_wall_time_ms: self.max_wall_time_ms.min(remaining_ms),
                    ..self.clone()
                }
            }
        }
    }
}
```

---

## 5. Budget & Cancellation

### Budget-Enforcement

Das Budget wird im Child-Turn-Loop erzwungen. Bei Überschreitung eines Limits wird der Child-Loop sofort mit `OpError::Execution("budget exceeded: <dimension>")` beendet. Der Parent bekommt diesen Fehler als strukturierten Rückgabewert — nicht als Panic oder unerwartete Terminierung.

| Limit | Enforcement-Punkt |
|---|---|
| `max_tokens` | Nach jedem LLM-Call: akkumulierte Tokens prüfen |
| `max_tool_calls` | Vor jedem Tool-Dispatch im Child-Loop |
| `max_wall_time_ms` | Polled via Cancellation-Token (alle ~100ms) |

```
Child-Turn-Loop:
  ┌─────────────────────────────────────────┐
  │  LLM-Call → Tokens akkumulieren         │
  │  if tokens > max_tokens → break(budget) │
  │                                         │
  │  Tool-Dispatch                          │
  │  if tool_calls > max_tool_calls → break │
  │                                         │
  │  Cancellation-Token prüfen              │
  │  if cancelled → break(cancelled)        │
  └─────────────────────────────────────────┘
```

### Cancellation

Der Parent besitzt einen `CancellationToken`. Wenn der Parent-Loop abbricht (Benutzer-Abbruch, eigener Timeout, Fehler), wird ein Signal über `cancellation_token.child()` an den Child-Token propagiert.

Der Child-Loop pollt den Token mindestens einmal pro Iteration (nach jedem LLM-Response). Die maximale Latenz zwischen Cancellation-Signal und Child-Stopp beträgt **unter 100ms** im Normalfall (ohne blockierenden I/O).

```rust
// Im Child-Turn-Loop, nach jedem LLM-Response:
if child_ctx.cancellation_token().is_cancelled() {
    return Err(OpError::Cancelled);
}
```

### Timeout-Propagation

```
parent_deadline vorhanden?
  ├── Nein → child_budget unverändert
  └── Ja  → child_budget.max_wall_time_ms = min(
                 child_budget.max_wall_time_ms,
                 parent_deadline - Instant::now()
             )
```

Das stellt sicher, dass ein Child niemals länger läuft als sein Parent noch Zeit hat — auch wenn das Child-Budget großzügig konfiguriert ist.

---

## 6. Transcript-Einbettung

### Standardverhalten (kein Transcript)

Nur das finale `OpOutput` des Childs fließt in den Parent-Context. Der interne Verlauf des Childs (Tool-Calls, Zwischenergebnisse, LLM-Turns) ist für den Parent unsichtbar.

```
Parent-Context:
  [... vorherige Turns ...]
  Tool-Call: spawn_researcher(input)
  Tool-Result: { summary: "...", confidence: 0.87 }   ← nur das
  [nächster Turn ...]
```

### Optionales Transcript (`include_transcript: true`)

Wenn aktiviert, wird der vollständige Child-Transcript als verschachteltes Objekt in den `OpOutput` eingebettet und fließt damit in den Parent-Context. Das erhöht die Token-Kosten erheblich, kann aber für Debugging oder für Fälle nützlich sein, wo der Parent den Reasoning-Pfad des Childs kennen muss.

```
Parent-Context:
  [... vorherige Turns ...]
  Tool-Call: spawn_researcher(input)
  Tool-Result: {
    summary: "...",
    transcript: [
      { role: "user", content: "..." },
      { role: "assistant", content: "...", tool_calls: [...] },
      { role: "tool", content: "..." },
      { role: "assistant", content: "..." }   ← finaler Child-Turn
    ]
  }
```

### Redaction-Regel (nicht verhandelbar)

Unabhängig von `include_transcript` und unabhängig von `--log-sensitive`-Flags:

- **System-Prompts des Childs werden NICHT geloggt** und erscheinen nicht im eingebetteten Transcript.
- **Tool-Argumente, die als `sensitive: true` markiert sind, werden NICHT in den Transcript eingebettet** — sie werden durch `[REDACTED]` ersetzt.
- Diese Regel gilt auch für den Child-Transcript im Parent-Context: die Sicherheitsgrenze zwischen Parent und Child bleibt bestehen, auch wenn Transcript-Einbettung aktiv ist.

Der Grund: ein Parent-Agent könnte kompromittiert sein. Der System-Prompt des Childs ist seine Autorisierungsbasis — er darf nicht über den eingebetteten Transcript nach oben lecken.

---

## 7. Parent/Child-Fencing bei Crash/Resume

### Child-Crash während normaler Ausführung

Der `ManagedAgentSpawner` fängt alle Child-Panics und Fehler und konvertiert sie in strukturierte `OpError`-Varianten:

| Child-Fehler | Parent bekommt |
|---|---|
| Unbehandelte Panic | `OpError::Execution { class: "panic", message: "..." }` |
| Budget überschritten | `OpError::Execution { class: "budget_exceeded", ... }` |
| Tool-Fehler im Child | `OpError::Execution { class: "tool_failure", ... }` |
| Netz-Timeout im Child | `OpError::Execution { class: "timeout", ... }` |

Der Parent kann auf jeden dieser Fehler reagieren und entscheiden, ob er retried, fallback ausführt oder den Fehler nach oben propagiert.

### Parent-Crash während Child läuft

Wenn der Parent-Prozess unerwartet endet (SIGKILL, OOM, Panic), wird `ManagedAgentSpawner::cancel_all_children()` über einen `Drop`-Handler aufgerufen. Dieser:

1. Sendet das Cancellation-Signal an alle laufenden Child-Tokens.
2. Wartet maximal **500ms** auf ein sauberes Child-Ende.
3. Beendet danach verbleibende Child-Prozesse forciert (SIGTERM → SIGKILL).

### Resume nach Neustart

Nach einem Crash/Neustart wird nur die **Parent-Session** wiederhergestellt. Child-Agents werden **nicht** wiederhergestellt. Begründung:

- Die Authority des Childs ist an den ursprünglichen Parent-Call gebunden. Nach einem Neustart des Parents ist dieser Call nicht mehr aktiv — ein wiederhergestellter Child hätte eine Autorität ohne gültigen Auftraggeber.
- Ein "Reissue" der Authority würde erfordern, den ursprünglichen `OpContext` zu verifizieren, was nicht sicher möglich ist ohne eine vollständige Replay-Kette.
- Stattdessen: der Parent-Loop startet nach dem Resume an dem Punkt, wo er den Child-Tool-Call abgesetzt hat, und spawnt den Child neu, falls nötig.

```
Parent-Resume-Ablauf:
  1. Parent-Session laden (letzter bekannter Zustand)
  2. Falls Tool-Call für AgentTool im letzten Turn: Child gilt als "nicht gestartet"
  3. Parent-Loop fährt an diesem Punkt fort → spawnt Child neu (frisches Budget)
```

---

## 8. Registrierung via `#[operation]`-Makro

Der `AgentToolAdapter` integriert sich über das bestehende `#[operation]`-Makro. Ein `agent_tool(...)`-Attribut löst die automatische Adapter-Generierung aus — kein manueller `impl`-Block notwendig.

```rust
/// Delegiert eine Recherche-Aufgabe an den Sub-Agent "researcher".
/// Budget: 8.000 Tokens, 20 Tool-Calls, 30 Sekunden Wandzeit.
///
/// Nur für Operator-Permissions zugänglich; der Child läuft im Read-Only-Modus.
#[operation(
    name = "spawn_researcher",
    summary = "Delegiert an Sub-Agent 'researcher' mit Budget 8k Tokens.",
    domain = "agents",
    permission = "operator",
    agent_tool(
        child = "researcher",
        authority = "reduce_to_read_only",
        budget = "8k_tokens,20_tool_calls,30s",
        include_transcript = false,
    ),
)]
async fn spawn_researcher(ctx: &OpContext, input: OpInput) -> Result<OpOutput, OpError> {
    /* Body wird vom `#[operation]`-Makro generiert — kein manueller Impl nötig.
       Das Makro instanziiert AgentToolAdapter mit den Parametern aus dem Attribut
       und delegiert den `invoke`-Aufruf. */
}
```

### Makro-generierter Code (konzeptuell)

Das `#[operation]`-Makro mit `agent_tool(...)` expandiert ungefähr zu:

```rust
async fn spawn_researcher(ctx: &OpContext, input: OpInput) -> Result<OpOutput, OpError> {
    static ADAPTER: std::sync::OnceLock<AgentToolAdapter> = std::sync::OnceLock::new();
    let adapter = ADAPTER.get_or_init(|| {
        AgentToolAdapter::new(
            Arc::clone(AgentRegistry::get("researcher")),
            reduce_to_read_only,   // Funktion aus dem `authority`-Parameter
            AgentBudget {
                max_tokens: 8_000,
                max_tool_calls: 20,
                max_wall_time_ms: 30_000,
            },
            false,   // include_transcript
        ).expect("AgentToolAdapter-Initialisierung fehlgeschlagen")
    });
    adapter.invoke(ctx, input).await
}
```

### `authority`-Funktionen

Die `authority`-Parameter im Makro referenzieren benannte Funktionen im Crate-Scope:

```rust
/// Reduziert die SandboxSpec auf Read-Only: entfernt alle Write- und Execute-Permissions.
fn reduce_to_read_only(parent: &SandboxSpec) -> SandboxSpec {
    parent.clone().without_permissions(&[
        Permission::FileWrite,
        Permission::FileDelete,
        Permission::ProcessSpawn,
        Permission::NetworkWrite,
    ])
}
```

---

## 9. Nicht in Welle 4

Die folgenden Features sind bewusst aus Welle 4 ausgeschlossen. Sie werden in späteren Wellen behandelt oder sind grundsätzlich nicht geplant.

### Agent-Self-Modification

Ein Agent kann zur Laufzeit **nicht** seinen eigenen System-Prompt, seine Tool-Liste oder seine Sandbox-Konfiguration ändern. Das schließt auch Child-Agents ein, die über den `AgentToolAdapter` gespawnt werden. Self-Modification würde die Authority-Monotonie gefährden und ist konzeptionell ausgeschlossen.

### Cross-Session-Child-Wiederverwendung

Ein Child-Agent, der in einer Session gespawnt wurde, kann in einer anderen Session **nicht** wiederverwendet werden. Jeder `AgentToolAdapter::invoke`-Aufruf spawnt einen frischen Child. Pooling und Wiederverwendung von Childs (z.B. für häufig aufgerufene Recherche-Agents) folgt in Welle 5, wenn das Cost-Accounting-Fundament steht.

### Explizite Cost/Token-Accounting-Persistierung

Token-Kosten und Tool-Call-Counts des Childs werden in Welle 4 **nicht** persistiert. Sie sind in der `AwaitingChild`-Antwort enthalten und können vom Parent ausgelesen werden, aber es gibt noch keine Datenbankschicht, die diese Daten über Sessions hinweg aggregiert. Das folgt in Welle 5.

### Bidirektionale Kommunikation während Child-Ausführung

Der Parent kann dem Child **nicht** während dessen Ausführung Nachrichten schicken. Das Muster ist: Aufruf → Warten → Ergebnis. Streaming-Zwischenergebnisse sind nicht vorgesehen.

---

## 10. Testing-Plan

### Unit-Tests

#### Authority-Reducer ist monoton (Property-Test)

```
Property: Für alle SandboxSpec S und alle authority_reducer-Funktionen f gilt:
  f(S).is_subset_of(S) == true

Testansatz:
  - QuickCheck / proptest mit zufälligen SandboxSpec-Instanzen
  - Alle registrierten authority_reducer-Funktionen werden geprüft
  - Mindestens 1.000 Iterationen pro Funktion
```

#### Authority-Verletzung beim Konstruktor

```
Test: AgentToolAdapter::new mit einer authority_reducer-Funktion, die Rechte hinzufügt
Erwartetes Ergebnis: Err(OpError::AuthorityViolation)
```

#### Budget-Clamp an Deadline

```
Test: AgentBudget::clamp_to_deadline mit parent_deadline < budget.max_wall_time_ms
Erwartetes Ergebnis: effective_budget.max_wall_time_ms == remaining_ms
```

### Integrationstests

#### Budget-Überschreitung → `OpError::Execution`

```
Setup:
  - AgentBudget { max_tokens: 100, max_tool_calls: 2, max_wall_time_ms: 5_000 }
  - Child-Agent, der absichtlich > 100 Tokens generiert (Mock-LLM)

Test:
  AgentToolAdapter::invoke(ctx, input).await

Erwartetes Ergebnis:
  Err(OpError::Execution { class: "budget_exceeded", message: "max_tokens: 100" })
```

#### Parent-Cancel → Child sieht `Cancelled` innerhalb < 100ms

```
Setup:
  - Child-Agent mit einer langen Mock-LLM-Antwort (simuliert 500ms Latenz)
  - Parent-CancellationToken

Ablauf:
  1. AgentToolAdapter::invoke starten (Hintergrund-Task)
  2. Nach 50ms: cancellation_token.cancel()
  3. Ergebnis des invoke-Calls messen

Erwartetes Ergebnis:
  - invoke gibt Err(OpError::Cancelled) zurück
  - Gesamtzeit zwischen cancel() und Err: < 100ms (gemessen)
```

#### Authority-Verletzung zur Laufzeit

```
Setup:
  - Parent-Sandbox: Observer-Level (minimale Permissions)
  - authority_reducer: gibt Parent-Sandbox unverändert zurück (korrekt)
  - Manueller Eingriff: child_sandbox wird nachträglich mit zusätzlichen Permissions versehen

Test: invoke() mit manipulierter child_sandbox

Erwartetes Ergebnis:
  Err(OpError::AuthorityViolation(...))
```

#### Transcript-Redaction

```
Setup:
  - Child-Agent mit einem Tool-Call, das `sensitive: true`-Argumente hat
  - include_transcript: true

Test: Prüfe, ob der eingebettete Transcript die sensiblen Argumente enthält

Erwartetes Ergebnis:
  tool_call.arguments["api_key"] == "[REDACTED]"
```

#### End-to-End: Parent spawnt Child, bekommt strukturiertes Ergebnis

```
Setup:
  - Mock-Child-Agent "researcher" mit definierten Antworten
  - Parent-Agent mit spawn_researcher-Tool

Ablauf:
  1. Parent-LLM entscheidet sich für Tool-Call spawn_researcher
  2. AgentToolAdapter::invoke wird aufgerufen
  3. Child-Agent läuft durch (Mock-LLM, 2 Turns)
  4. Ergebnis fließt zurück in Parent-Context

Erwartetes Ergebnis:
  - OpOutput.content enthält das Recherche-Ergebnis
  - Parent-Loop fährt mit nächstem Turn fort
  - Child-Session ist beendet und aus ManagedAgentSpawner entfernt
```

---

*Dieses Dokument beschreibt die Design-Entscheidungen für Wave 4. Implementierungsdetails (konkrete Typen, Trait-Signaturen, Makro-Expansion) können bei der tatsächlichen Implementierung abweichen — dieses Doc ist die normative Quelle für das Verhalten, nicht für die exakte API-Oberfläche.*
