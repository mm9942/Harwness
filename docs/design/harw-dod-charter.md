# `harw-dod`: Charta der Fassade

**Status:** Entwurf zur Diskussion, noch nicht normativ
**Zweck dieses Dokuments:** Warum diese Fassade existiert, wie sie heißt,
warum sie so heißt, was sie darf und was sie ausdrücklich nicht darf
**Verwandt:** `harw-security-observability-plan.md`, `harw-context-plan.md`,
`harw-dod-integration-and-dependencies.md`

---

## 1. Warum überhaupt eine Fassade

Das Sicherheitssubsystem besteht aus acht Crates über vier Ebenen. Ohne
Fassade müsste jeder Konsument die Schichtung kennen, aus welchem Crate
`Finding` kommt, aus welchem `WardenAction`, welche Version welches Schemas
gerade gilt. Das ist genau der Zustand, den `harw` für das SDK aufgelöst hat:
eine Fassade auf L6 mit einem Prelude, hinter der die interne Schichtung
umbaubar bleibt.

Für Verteidigungscode ist das mehr als Bequemlichkeit. Eine Fassade ist eine
**Reduktionsfläche**: sie legt fest, was von außen überhaupt erreichbar ist,
und alles, was sie nicht reexportiert, existiert für Konsumenten nicht. Bei
einem Subsystem, dessen halber Sinn darin besteht, Erreichbarkeit zu
begrenzen, ist das kein Komfortmerkmal, sondern die Umsetzung von S1 auf
Modulebene.

---

## 2. Der Name

`harw-dod` folgt dem Muster von `harw`: kurzer Name, klare Zugehörigkeit,
keine Beschreibung der Interna. Für das Kürzel gilt eine Festlegung, die
diese Charta trifft:

> **DoD steht in diesem Projekt für Detect, Orient, Defend.**

Das ist kein Wortspiel um seiner selbst willen. Die drei Silben sind die drei
Schichten des Subsystems, und sie stehen in genau der Reihenfolge, in der ein
Befund sie durchläuft:

**Detect** ist `harw-sensor`: lesen, messen, Ereignisse erzeugen. Keine
Bewertung, keine Privilegien über Lesegruppen hinaus.

**Orient** ist `harw-rules` plus die agentische Triage: einordnen. Was ist
Vertragsverletzung, was Schwellwert, was Abweichung. Hier lebt das Wissen,
die Baseline, die Historie, das Urteil.

**Defend** ist `harw-escalate` plus `harw-warden`: die Leiter und der
Durchsetzer. Reversibel vor irreversibel, geschlossene Aktionsmenge,
Audit-Pflicht.

Die Anspielung auf die OODA-Schleife (Observe, Orient, Decide, Act) ist
gewollt. Und das Entscheidende ist, was im Kürzel **fehlt**:

> **Das A für Act steht bewusst nicht im Namen.**

Handeln in seiner irreversiblen Form gehört dem Menschen. Kill, Rollback,
Entzug von Berechtigungen laufen niemals automatisch (S5, S11). Das Kürzel
beschreibt exakt die drei Schritte, die das System allein gehen darf, und
lässt den vierten weg. Wer den Namen liest und die Charta kennt, weiß damit
sofort, wo die Grenze liegt.

**Was der Name nicht bedeutet.** Kein Department, keine Behörde, kein
Machtzentrum. Die Fassade ist ein Reexport-Modul ohne eigene Logik und ohne
eigene Rechte. Sie ist der schmalste Teil des Systems, nicht der mächtigste.
Das ist wichtig genug, um es in die Crate-Doku zu schreiben, weil das Kürzel
sonst eine Erwartung weckt, die die Architektur ausdrücklich nicht erfüllt.

---

## 3. Was die Fassade ist

```rust
//! `harw-dod` — Detect, Orient, Defend.
//! Fassade des Sicherheits- und Verteidigungssubsystems.
//!
//! Diese Crate enthält KEINE Logik. Sie reexportiert Typen und
//! Einstiegspunkte und hält das Prelude. Wer Verhalten ändern will,
//! ändert es in der Crate, die es besitzt.

pub mod detect {   // aus harw-signals, harw-sensor
    pub use harw_signals::{SecurityEvent, EventKind, Actor, HostSample, SampleScope};
    pub use harw_sensor::{Sensor, SensorCapability, SensorHandle, Unbound, Bound};
}

pub mod orient {   // aus harw-signals, harw-rules
    pub use harw_signals::{Finding, Raw, RuleChecked, Triaged, Hardness, Severity,
                           Verdict, Assessment, ProposedAction};
    pub use harw_rules::{Rule, RuleContext, Baseline, Expectation, BaselineIndex};
}

pub mod defend {   // aus harw-escalate, harw-warden-proto
    pub use harw_escalate::{Action, Proposed, Authorized, Ladder, Freeze,
                            FreezeActive, FreezeResolved, dispatch};
    pub use harw_warden_proto::{WardenAction, WardenRequest, AuthorizationProof};
}

pub mod prelude { /* die zwanzig Typen, die man wirklich täglich braucht */ }
```

Der Zuschnitt in `detect`, `orient`, `defend` ist bewusst nicht der
Crate-Zuschnitt. Ein Konsument denkt in Phasen, nicht in Abhängigkeitsebenen.
Dass `Finding` in `harw-signals` wohnt und `Rule` in `harw-rules`, ist eine
Frage der Schichtung; dass beide zum Einordnen gehören, ist die Frage des
Nutzers.

---

## 4. Was die Fassade ausdrücklich nicht ist

**Kein Ort für Logik.** Kein `impl`, keine Funktion mit Verhalten, keine
Hilfsroutine, die sich irgendwo sonst nicht unterbringen ließ. Fassaden, die
anfangen, Dinge selbst zu erledigen, werden zu Sammelbecken.

**Kein Weg zum Warden.** `harw-dod` reexportiert `dispatch`, aber `dispatch`
nimmt ausschließlich `Action<Authorized>`, und dessen Konstruktor ist
`pub(crate)` in `harw-escalate`. Die Fassade kann diese Grenze nicht
aufweichen, weil sie nichts konstruieren kann, was sie nicht sieht. Das ist
S1 als Sichtbarkeitsregel, und die Fassade macht sie nicht auf.

**Kein Dach über der Infrastruktur.** `harw-context` und `harw-observe`
liegen nicht unter dieser Fassade. Kontext und Telemetrie sind für jeden
Agenten da, vom UIA bis zum Verifier. Sie unter die Sicherheitsfassade zu
ziehen würde zwei Fehler auf einmal machen: Infrastrukturänderungen sähen wie
Sicherheitsänderungen aus, und die Fassade wäre nicht mehr die schmale
Reduktionsfläche, als die sie gedacht ist.

**Kein privilegierter Prozess.** `harw-warden` ist ein eigenes Binary mit
eigenem Abhängigkeitsbudget. Die Fassade linkt seine Wire-Typen
(`harw-warden-proto`), nie seine Implementierung.

---

## 5. Intentionen: warum wir überhaupt so bauen

Dieser Abschnitt gehört in die Charta, weil jede der acht Crates leichter zu
verstehen ist, wenn man die Absicht dahinter kennt. Sechs Sätze, aus denen
alles andere folgt.

**Erstens: Unmöglichkeit ausdrücken, nicht Korrektheit prüfen.** Eine Prüfung
kann vergessen werden, ein Typ nicht. Deshalb kann `PermissionSet` nur
schneiden, `EgressSet` nur schneiden, `ContextBudgetSpec` nur verengen und
`Action<Authorized>` nur an einem Ort entstehen. Die Präzedenz ist im eigenen
Haus: `can_spawn` war dokumentiert und wurde nie aufgerufen, die Capabilities
konnten nie umgangen werden, weil es dort nichts zu umgehen gab.

**Zweitens: strukturell verhindern schlägt überwachen.** Ein großer Teil
dessen, was klassische Sicherheitswerkzeuge erkennen wollen, kann hier gar
nicht erst stattfinden: ein Agent ohne Egress-Ziel erreicht es nicht, ein
Kind ohne Schreibrecht schreibt nicht, ein Fragment außerhalb der Ceiling
wird nicht gerendert. Überwachung ist die Antwort für den Rest, nicht die
erste Antwort.

**Drittens: vorschlagen ist nicht committen.** Das Muster zieht sich durchs
ganze Haus: der Dream-Job schlägt vor, die UIA definiert und committet nicht,
`reconcile` liefert Schritte und `apply` wendet terminale Goal-Übergänge
nicht an, und `validate_goal_action` weist Modell-Akteure ab. Die Triage
folgt demselben Muster: sie liefert ein Verdikt mit `ProposedAction`, die
Leiter autorisiert, der Mensch entscheidet über das Irreversible.

**Viertens: das Urteil ist agentisch, der Reflex ist deterministisch.**
Paketfilter, Ratenbegrenzung, seccomp, Integritätsprüfung bleiben Kernel und
Regelwerk, weil sie schnell, unbestechlich und unter Angriff verfügbar sein
müssen. Ein Sprachmodell lässt sich überreden, ein Paketfilter nicht. Was ein
Agent besser kann als bestehende Werkzeuge, ist die Schicht darüber:
Korrelation über Zeit, Triage von Rauschen, Erklärung von Drift. Genau dort
sitzt er, und nirgends darunter.

**Fünftens: der Sollzustand ist bekannt, also ist Abweichung entscheidbar.**
Das ist der Vorteil, den kein zugekauftes Sicherheitswerkzeug hat. Wir wissen,
welcher Agent welchen Flow erzeugen darf, welcher Vertrag für welchen Pfad
gilt, welcher Plan-Knoten gerade aktiv ist. Deshalb ist eine
Vertragsverletzung hier ein harter Befund und keine Heuristik, und deshalb
darf sie handeln, während eine Statistik nur warnen darf.

**Sechstens: gebaut wird für lange Betriebsdauer.** Deshalb reines Rust im
privilegierten Pfad, deshalb kein Kernelmodul, deshalb jede fremde Bibliothek
hinter einem eigenen Trait, deshalb ein Abhängigkeitsbudget mit Obergrenze.
Ein Verteidigungssystem, das bei jedem Kernel-Update bricht, verteidigt nicht,
es beschäftigt.

---

## 6. Was das System ausdrücklich nicht tun soll

Diese Liste gehört in die Charta, weil ein Sicherheitssubsystem ohne erklärte
Grenzen zwangsläufig wächst.

- **Keine Verhaltensprofile von Personen.** Änderungsattribution ja, damit
  Drift erklärbar und Rollback möglich ist. Kein Modell darüber, ob sich
  jemand normal verhält. Für menschliche Akteure endet die Leiter bei Melden
  und Protokollieren.
- **Keine eigene Schadcode-Erkennung.** Etablierte Werkzeuge laufen per
  Timer mit festen Argumenten, wir lesen Reports. Wir bauen die Triage, nicht
  den Scanner.
- **Kein Kernelmodul.** Alle benötigten Fähigkeiten sind über stabile
  Schnittstellen erreichbar. Ein Out-of-Tree-Modul gäbe genau die
  Versionsstabilität auf, die Punkt sechs oben zum Ziel erklärt.
- **Kein generischer Ausführungspfad.** Es gibt keine Warden-Operation, die
  Text als Befehl nimmt. Wenn eine neue Fähigkeit gebraucht wird, ist sie
  eine neue benannte Aktion mit Zulässigkeitsregel und Audit-Namen, oder sie
  existiert nicht.
- **Kein eigener Zustand für Plan und Ziel.** Findings leben in
  `harw-knowledge`, Plan-Wirkung läuft ausschließlich über
  `harw-plan-bridge`. Die Fassade besitzt nichts.

---

## 7. Aufnahmekriterien für die Fassade

Damit `harw-dod` schmal bleibt, gilt für jeden Reexport:

1. Der Typ gehört zu einer der drei Phasen. Was in keine passt, gehört
   wahrscheinlich nicht ins Subsystem.
2. Der Typ ist Teil eines Vertrags, nicht einer Implementierung. `Rule` ja,
   die konkrete Schwellwertregel nein.
3. Der Typ eröffnet keinen Weg an einer Invariante vorbei. Insbesondere kein
   Konstruktor für `Action<Authorized>`, kein Weg an der Ladder vorbei, kein
   direkter Warden-Client ohne Proof.
4. Der Reexport ist versioniert wie das Subsystem. Ein Schemawechsel ist eine
   Fassadenänderung und wird als solche im Changelog geführt.

Ein Reexport, der eines dieser Kriterien verletzt, wird abgelehnt, auch wenn
er bequem wäre. Bequemlichkeit ist der übliche Weg, auf dem Fassaden ihre
Reduktionswirkung verlieren.
