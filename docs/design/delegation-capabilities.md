# Delegationscapabilities und lokaler Organisationsgraph

## Zweck

Eine organisatorische Rolle ist eine globale Sicherheitsobergrenze, aber kein
Agentenkatalog und keine Modelloberfläche. Ein Agent darf nur Zielagenten
kennen und anfordern, für die ihm seine eigene, bereits geprüfte Definition
zur Laufzeit eine konkrete Delegationscapability ausstellt.

Dieser Vertrag verhindert zwei unterschiedliche Fehlerklassen:

1. Ein Modell kann eine vorhandene, aber für seine Position nicht bestimmte
   Rolle erraten oder aus einer Fehlermeldung ableiten.
2. Eine pauschale Berechtigung `ChildOrchestrator → ChildOrchestrator` wird
   irrtümlich zu einer rekursiven, unbeschränkten Delegationsbefugnis.

## Begriffe

- **Organisationsrolle**: geschlossene Kategorie (`UserInterface`,
  `RootOrchestrator`, `ChildOrchestrator`, `Worker`, `UiaWorker` — Addendum J,
  `AgentSteward` — Addendum K). Die Rollenmatrix ist nur eine notwendige obere
  Grenze.
- **Agentendefinition**: versionierte, vertrauenswürdig eingebettete
  Spezialisierung mit Rolle, Tool-Fläche, Budget und Spawn-Vertrag.
- **Delegationscapability**: konkrete, von einem Parent übertragene Befugnis,
  genau eine Zieldefinition innerhalb engerer Grenzen zu starten.
- **Child-Lease**: die nach erfolgreicher Admission erzeugte Laufzeitressource
  eines Kindes. Ein Lease entsteht nie direkt aus Modell-JSON.

## Sichtbarkeit

Die sichtbare Delegationsmenge eines Agenten ist:

```text
DefinitionDeclaredTargets
∩ RoleMatrixTargets
∩ ParentGrantedTargets
∩ RemainingDepth
∩ RemainingBudget
∩ AuthorityCeiling
∩ ContextCeiling
∩ ReadWriteScopeCeiling
```

Nur diese geschnittene Menge wird als Tool-Parameter und als lokales
Organisationswissen exponiert. Ist sie leer, gibt es keine Spawn-/Delegations-
Oberfläche und keinen Agentenkatalog.

Die Schnittmenge ist monoton: Kein Kind darf über mehr Autorität, Kontext,
Netz-, Dateisystem- oder Toolrechte, Budget oder Resttiefe als sein Parent
verfügen.

## Rollenprojektionen

| Rolle | Darf über Delegation wissen |
| --- | --- |
| UIA | Nur ausdrücklich ausgestellte Einstiegscapabilities, normalerweise `activate-root`. Keine Worker- oder Sub-Orchestrator-Namen. |
| Root-Orchestrator | Seine eigenen, konkreten Worker- und Subtree-Capabilities sowie die Verantwortung für Ziel, Budget und Synthese. |
| Sub-Orchestrator | Ausschließlich sein delegierter Teilbaum, lokale Worker und explizit weitergereichte Subtree-Capabilities. Keine Geschwister oder Root-exklusiven Ziele. |
| Worker | Keine dauerhafte Delegation und kein Agentenkatalog. |
| `uia-worker` (`UiaWorker`, Addendum J) | Eigene, vollständig abgekapselte Organisationsrolle; nur die UIA darf ihn spawnen, er selbst hat keine dauerhafte Delegation und keinen Agentenkatalog. |
| `agent-steward` (`AgentSteward`, Addendum K) | Interner Agent, der das Wissen über Agentendefinitionen explizit besitzt und umsetzt; sowohl die UIA als auch der Root-Orchestrator dürfen ihn spawnen, er selbst hat keine dauerhafte Delegation und keinen Agentenkatalog. |

## Rekursive Sub-Orchestrierung

`ChildOrchestrator → ChildOrchestrator` in der Rollenmatrix bedeutet nicht,
dass jeder Child-Orchestrator weitere Child-Orchestratoren starten kann. Eine
solche Admission benötigt zusätzlich eine exakte Zieldefinition, die

1. in der Definition des Parents deklariert ist,
2. vom Parent tatsächlich an diesen Lauf weitergereicht wurde und
3. innerhalb aller verbleibenden Decken liegt.

Die Runtime behandelt ein nicht vorhandenes Ziel als nicht sichtbar. Sie darf
bei einer Ablehnung keine Namen, Rollen oder sonstigen Eigenschaften weiterer
registrierter Agenten offenlegen.

## UIA-Routing

Die UIA ist ein lokaler, interaktiver Eintrittspunkt. Sie erstellt keinen
Worker direkt. Delegierbare Arbeit wird als Arbeitsabsicht an eine sichtbare
Root-Capability gegeben:

```text
UIA → Root-Orchestrator → Worker oder begrenzter Sub-Orchestrator-Teilbaum
```

Damit ist eine abgelehnte UIA→Worker-Admission kein normaler Laufzeitpfad,
sondern ein nicht exponierbarer Zustand.

## Migrationszustand

Die bestehende Rollenmatrix und die exakte
`allowed_child_orchestrators`-Prüfung bleiben während der Migration als
fail-closed Admission-Grenze bestehen. Folgeschritte ersetzen die freie
rollenbasierte Modell-Eingabe durch capability-identifizierte Requests und
projizieren die sichtbare Menge in Tool-Schemas und Kontextprogramme.
