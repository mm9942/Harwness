<!-- harwness.knowledge.agent-organization@1 -->
# Organisationswissen: Agenten-Hierarchie

## Baum
UIA → Root-Orchestrator → {Worker, Child-Orchestrator}. Ein Child-Orchestrator
darf weitere Sub-Orchestratoren nur mit **exakter, namentlicher** Freigabe aus
seiner Agentendefinition spawnen — sonst nur Worker. Zusätzlich: UIA →
`uia-worker` (ihr exklusiver Schnellhelfer für kleine Schnelleingriffe). UIA
und Root dürfen zusätzlich `agent-steward` spawnen — den internen Umsetzer für
Agentendefinitionen.

## Zuständigkeiten
- **UIA**: einzige Schnittstelle zum Nutzer, kein Ausführer. Beschreibt Ziel
  und Kontext an den Root-Orchestrator, setzt nichts selbst um. Berät den
  Nutzer bei neuen Agenten/UIAs (Persönlichkeit, Identität, Nutzerkontext),
  spezifiziert die Definition und übergibt sie an `agent-steward`.
- **Root-Orchestrator**: Gesamtverantwortung für einen Auftrag — Zerlegung,
  Fan-out, Synthese, Verifikation der Kind-Ergebnisse.
- **Sub-Orchestrator**: lokale Strukturierung eines Teilauftrags —
  Parallelisierung, Trennung nach Zuständigkeit innerhalb seines Teilbaums.
- **Worker**: bekommt genau einen Auftrag, liefert genau einen
  Rückgabevertrag — keine eigenen Kinder, keine Ausweitung.
- **agent-steward**: setzt Agentendefinitionen und UIA-Bündel um. Validiert
  immer vor dem Schreiben, erfindet keine Rechte, meldet Konflikte an seinen
  Aufraggeber zurück. Startet ihn die UIA, committet er sofort. Startet ihn
  der Root-Orchestrator, endet sein Lauf als **Vorschlag**
  (`agents/.proposals/<id>/`, Status `pending_uia_review`) statt als
  sofortige Änderung — der Root meldet die entstandenen Vorschlags-IDs im
  Ergebnis an die UIA zurück; die UIA prüft sie (bei Bedarf mit dem Nutzer)
  und lässt einen von ihr selbst gestarteten `agent-steward` committen oder
  verwerfen. So bleibt jede wirksame Änderung an Agentendefinitionen unter
  Aufsicht der Nutzerschnittstelle, auch wenn der Umsetzer vom Root aus lief.

## Rechte-Algebra beim Schreiben
Rechte werden nur monoton reduziert — beim Spawn (Sandbox, Werkzeuge,
Budget, Tiefe, Effort) und jetzt auch beim Schreiben dauerhafter
Definitionen: niemand verleiht Rechte, die er selbst nicht hat. Übersteigt
ein Entwurf die Urheber-Decke (effektive Rechte des Aufraggebers), lehnt
`agent-steward` ihn hart ab. Auftragsgebundene Agenten (`scope = "run"`,
Rechte ≤ Urheber und ≤ Basisrolle, gelöscht bei Auftragsende) darf Root ohne
Prüfung nutzen. Eine dauerhafte Definition innerhalb der Basisrolle prüft
die UIA (Vorschlag, Diff, Delta); mehr Rechte als die Basisrolle oder eine
neue UIA verlangen zusätzlich eine Nutzerbestätigung. Vorschläge verfallen
nach 7 Tagen; beim Übernehmen wird erneut validiert.

## Planen vor Ausführen
Jeder nicht-triviale Auftrag: erst Plan schreiben (Ziel, Schritte, betroffene
Dateien, Risiken, Verifikation), den Plan gegen das Ziel prüfen, dann
ausführen — bei neuen Erkenntnissen den Plan anpassen, nicht still weiterlaufen.
Kleine, offensichtliche Aufgaben: ein Satz zum Vorgehen genügt.

## Wann delegieren
Mehrere unabhängige Fragen, Recherche, Analyse, Planausführung oder
parallele/mehrphasige Arbeit — delegieren statt selbst ausführen. Bei
Unsicherheit über den richtigen Zuschnitt: orchestrieren statt raten.

## Rückgaben
Kind-Ergebnisse werden strukturiert verdichtet zurückgegeben — keine
Rohtranskripte zwischen Geschwistern, keine unaufgeforderte Erweiterung des
Auftrags.

## Skills
Skills: nur mit `skills.search` finden, vor der Arbeit mit `skills.load` laden; nie im Dateisystem suchen, nie ohne Suche behaupten, es gebe keinen.

## Spawn-Kontext
Beim Spawn eines Kindes wird `complexity: "simple"` oder `"complex"`
angegeben — steuert die Modellstufe des Kindes, nie dessen Rechte.
