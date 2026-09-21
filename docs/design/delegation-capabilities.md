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

## Root vs. Sub-Orchestrator: positionell, nicht kategorisch

Projektsetzung (bindende Vorgabe des Projektinhabers, nicht Gegenstand
dieser Analyse):

> Ein Root-Orchestrator ist ein Orchestrator, der nur deshalb so heißt, weil
> es unter ihm Sub-Orchestratoren gibt.

Die Unterscheidung `RootOrchestrator`/`ChildOrchestrator` ist damit
**positionell** (Ort im Baum: besitzt der Agent selbst einen Orchestrator-
Parent, oder ist er die Wurzel des aktuellen Auftrags), nicht **kategorisch**
(keine grundverschiedene Rechteklasse mit Fähigkeiten, die der jeweils
anderen Rolle grundsätzlich verwehrt sind).

### Aktuelle Abweichungen von der Zielarchitektur

Der heutige Code-Stand behandelt beide Rollen an drei Stellen als
kategorisch verschieden. Das sind keine Fehler und keine Rechtfertigung der
Trennung — nur der dokumentierte Ist-Zustand gegenüber der oben zitierten
Zielsetzung:

1. **Spawn-Matrix — `AgentSteward` nur von `RootOrchestrator`.**
   `harw-agent-dsl/src/roles.rs`, Funktion `can_spawn` (ab Zeile 92): Zeile
   106 gewährt `(RootOrchestrator, AgentSteward) => true`, der
   `ChildOrchestrator`-Block (Zeilen 111–113) trägt jedoch keinen
   `AgentSteward`-Arm und fällt auf `(ChildOrchestrator, _) => false`
   zurück. Ein `ChildOrchestrator` kann `AgentSteward` also nicht spawnen,
   selbst wenn er positionell als Root für seinen eigenen Teilbaum
   fungiert.
2. **Unterschiedliche Standard-Reasoning-Effort-Gewichte.**
   `harw-core/src/child_controller.rs`, `struct RoleEffortWeights` (ab Zeile
   848) mit `impl Default for RoleEffortWeights` (Zeilen 857–868):
   `root_orchestrator: High`, `root_orchestrator_with_subs: Medium`,
   `sub_orchestrator: Medium`. `RoleEffortWeights::for_child` (Zeilen
   886–913) liest bei `ChildOrchestrator` immer `self.sub_orchestrator`
   (fest `Medium`), während `RootOrchestrator` nur dann auf `Medium`
   absinkt, wenn er selbst Sub-Orchestrator-Freigaben trägt
   (`has_child_orchestrator_grants`). Ein `ChildOrchestrator` ohne eigene
   Sub-Orchestrator-Freigaben bekommt damit dasselbe Gewicht wie ein
   `RootOrchestrator` MIT solchen Freigaben — nicht dasselbe wie ein
   `RootOrchestrator` ohne sie.
3. **`ChildOrchestrator` hat keine eingebaute Instanz.** Unter
   `harw-registry-defaults/agents/` trägt aktuell kein Agent
   `role = "child-orchestrator"` als tatsächliche Rollenzuweisung. Der
   String taucht nur in zwei Begründungskommentaren auf —
   `harw-registry-defaults/agents/families/security/security.toml:236` und
   `harw-registry-defaults/agents/organization/default.toml:109` — beide
   erklären ausdrücklich, warum die dort beschriebene Leader-Rolle
   (`analyst` bzw. `context-steward`) stattdessen `role = "worker"` trägt
   und nicht `role = "child-orchestrator"`. Die Rolle existiert damit nur im
   Typsystem (`AgentRoleId::ChildOrchestrator`) und in der Spawn-Matrix,
   nicht in einer gelebten Konfiguration.

### Kein Umbau in dieser Welle

Eine Zusammenführung zu einer einzigen Orchestrator-Rolle mit
Positionsmerkmal (statt zwei getrennten `AgentRoleId`-Varianten) ist ein
**separates, noch nicht begonnenes Umbau-Vorhaben**. Es berührt die
Spawn-Matrix (`can_spawn`) und ihre Tests in `harw-agent-dsl/src/roles.rs`
direkt sowie `RoleEffortWeights` in `harw-core/src/child_controller.rs`.

Dieses Vorhaben wird bewusst **nicht** parallel zum aktuell laufenden
Spawn-Delegations-Fix umgesetzt: Liefen beide Änderungen gleichzeitig, wäre
bei einem auftretenden Problem nicht mehr zuordenbar, ob die Ursache im
Delegations-Fix oder im Rollen-Umbau liegt. Die Vereinheitlichung folgt als
eigener, isoliert testbarer Schritt, sobald der Spawn-Delegations-Fix
abgeschlossen und verifiziert ist.
