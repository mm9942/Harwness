# Matrix-Szenarien entwerfen — Interview, TOML-Entwurf, Checkliste

**Regel:** Ein gutes Szenario beginnt mit einer **Frage**, nicht mit einer Geschichte. Jede Fraktion braucht ein **unterscheidbares Verhalten**, jede Information eine **klare Sichtbarkeit**, jedes Ziel eine **messbare Endbedingung**. Der Entwurf wird erst gezeigt, dann geprüft, dann – nur mit Freigabe – als Datei geschrieben.

**Warum:** Sprachmodelle als Spieler neigen dazu, sich alle gleich vernünftig und kompromissbereit zu verhalten, und verlieren ihre Rolle nach einigen Runden. Weiche Adjektive im Briefing helfen dagegen kaum; konkrete Regeln, ein Verlustrahmen und rote Linien schon. Unklare Sichtbarkeit führt zu Leaks, unmessbare Ziele zu einer beliebigen Auswertung.

Referenzen im Repository: `harw-matrix-game/scenarios/karst-islands.toml` (klassisch), `harw-matrix-game/scenarios/cloud-sme-2027.toml` (business), `docs/design/matrix-game.md` §5.

## Wann anwenden

- Jemand will ein neues Matrix Game aufsetzen oder ein vorhandenes Szenario schärfen.
- Ein Rohbriefing (Notizen, Mail, Stichpunkte) soll in ein spielbares Szenario überführt werden.
- Ein Lauf war „langweilig“ oder alle Seiten spielten gleich – dann Schritt 2 und 6 erneut durchgehen.

## Interview in sieben Schritten

Pro Nachricht ein Schritt, höchstens drei Fragen. Antworten sofort in Stichpunkten zurückspiegeln.

1. **Kernfrage und Zweck**
   - Welche Frage soll nach dem Spiel beantwortbar sein? (ein Satz → wird `purpose`)
   - Wozu dient das Spiel: Entscheidung vorbereiten, Schwachstellen finden, lernen?
   - Modus: `classic` (vier Fraktionen, Argumente, Tracks) oder `business` (Teams, Marktmodell, Schlüsselfragen)?
2. **Akteure mit unterscheidbarem Verhalten** (klassisch: genau vier)
   - Name, Ebene (vergleichbar: alle Staaten, alle Firmen …), öffentliches Briefing.
   - Öffentliche und geheime Ziele (je höchstens vier).
   - Machtmittel (`assets`).
   - Verhalten: Was tut die Fraktion **immer**, was **nie**? Wie viel Risiko nimmt sie für einen Durchbruch (0 = keins, 1 = alles)? Was verliert sie, wenn sie **nicht** handelt? Woran misst sie „normal“ (Anker)? Mindestens zwei rote Linien.
   - Probe: Würden zwei Fraktionen in derselben Lage dasselbe tun? Dann schärfen.
3. **Lage zu Spielbeginn**
   - Öffentliche Ausgangslage (drei bis sechs Sätze, konkret, keine Allgemeinplätze).
   - Tracks (Zahlenskalen mit `min`/`max`/`start`), Zustände (diskrete Werte), verdeckte Objekte, Projekte (höchstens drei Stufen).
   - Was stellt eine Runde dar, wie viele Runden (klassisch empfohlen sechs bis acht)?
4. **Informationsasymmetrie**
   - Welche Werte kennt nur ein Sitz (`seat:<id>`), welche nur der Schiedsrichter (`umpire`)?
   - Welche Paare dürfen sich privat absprechen?
   - Gibt es Unterlagen? Dann `[materials] dir = "…"` mit Unterordnern `geteilt/` (alle), `<sitz-id>/` (nur dieser Sitz), `paare/<a>+<b>/` (nur a und b, alphabetisch), `umpire/` (nur Schiedsrichter).
5. **Messbare Endbedingungen**
   - Jedes Ziel an einen Track-Wert, Zustand oder eine Projektstufe binden („`water` ≥ 1 bei Spielende“ statt „Versorgung sichern“).
   - Ende: feste Rundenzahl oder Schlussargumente.
6. **Leitplanken für den Schiedsrichter**
   - Maximaler Track-Sprung je Ergebnis (`max_track_step`), ausgeschlossene Ereignisse, Maßstab für Plausibilität.
   - Vorbereitete Injects (höchstens drei je Lauf, nie zwei in derselben Runde).
   - Sollen Verhandlungen für den Schiedsrichter sichtbar sein (`umpire_negotiations`)?
7. **Optionale Ergänzungen**
   - Verhaltensprofile als Feld (`[factions.behavior]`) statt nur im Briefing.
   - Red-Cell-Sitz, der in der Gegenargument-Phase die tragende Annahme angreift.
   - Unterlagen-Ordner.

Mini-Beispiel (frei erfunden): „aggressiv“ wird zu „eröffnet jede Verhandlung mit einer Forderung über dem eigentlichen Ziel; macht nie das erste Zugeständnis; Risiko 0,7; Verlustrahmen: jede Runde ohne Gewinn kostet Rückhalt im eigenen Lager; rote Linien: keine öffentliche Entschuldigung, kein Verzicht auf den Hafen“.

## TOML-Skelett (klassischer Modus)

Pflichtfelder sind die, die im Beispielszenario ohne Kommentar stehen. Felder mit **Engine-Vorbehalt** nur verwenden, wenn die installierte Engine sie kennt – der Loader lehnt unbekannte Felder ab. Im Zweifel die Verhaltensangaben zusätzlich ins `briefing` schreiben.

```toml
schema = "harwness.matrix-scenario/v1"
id = "<kurz-ohne-leerzeichen>"          # IDs ohne : , Leerzeichen [ ]
title = "<Titel>"
purpose = "<Kernfrage in einem Satz – erste Zeile jedes Prompts>"
seed = ""                                # leer = beim Start ziehen
rounds = 6
round_represents = "<z. B. etwa ein Monat>"

[rules]
argument_system = "pros_cons"            # | "three_reasons"
argument_mode = "simultaneous"           # | "sequential"
adjudication = "pros_cons_2d6"           # | "estimative_d100"
turn_order = "fixed"                     # | "leader_first"
max_track_step = 1
max_secret_arguments_per_seat = 1
ending = "fixed_rounds"                  # | "final_arguments"

[rules.negotiation]
enabled = true
max_exchanges = 2
max_channels_per_seat = 2
max_message_chars = 800

[visibility]
umpire_negotiations = "none"             # | "cited" | "full"
secret_outcome_public = false
leak_guard = "strict"                    # | "flag_only"

[world]
public_situation = """
<konkrete Ausgangslage>
"""
tracks = [
  { id = "<track>", label = "<Name>", min = -3, max = 3, start = 0, visibility = "public" },
  { id = "<geheim>", label = "<Name>", min = 0, max = 3, start = 1, visibility = "seat:<fraktion>" },
  { id = "<nur-schiedsrichter>", label = "<Name>", min = 0, max = 3, start = 1, visibility = "umpire" },
]
states = [
  { id = "<zustand>", label = "<Name>", values = ["a", "b", "umstritten"], start = "a", visibility = "public" },
]
objects = [
  { id = "<objekt>", label = "<Name>", hidden = true, protection = 1, visibility_when_found = "public" },
]
projects = [
  { id = "<projekt>", label = "<Name>", stages = 3, progress = 0, visibility = "public" },
]

[[factions]]                             # genau vier Blöcke
id = "<fraktion-a>"
name = "<Name>"
level = "<Ebene>"
briefing = "<öffentliches Briefing>"
goals.public = ["<messbar>"]
goals.secret = ["<messbar>"]
assets = ["<Machtmittel>"]

# Engine-Vorbehalt: Verhaltensprofil je Fraktion
# [factions.behavior]
# rules = ["<tut immer …>", "<tut nie …>"]
# risk = 0.6                             # 0..1
# loss_framing = "<was bei Untätigkeit verloren geht>"
# anchor = "<Referenzpunkt für normal>"
# red_lines = ["<nie …>", "<nie …>"]     # mindestens zwei

# … drei weitere [[factions]]

[seating]
order = ["<fraktion-a>", "<fraktion-b>", "<fraktion-c>", "<fraktion-d>"]

[[injects]]
id = "<inject>"
round = 3
audience = "public"                      # | "umpire" | "seat:x" | "seat+umpire:x" | "pair:a,b"
text = "<Ereignis>"
effects = [{ op = "add", var = "<track>", by = -1 }]

# Optional: Unterlagen (relativ zur Szenario-Datei, absolut oder mit ~)
# [materials]
# dir = "unterlagen"

# Engine-Vorbehalt: Red-Cell-Sitz ohne Siegbedingung
# [red_cell]
# sharpness = 0.5                        # 0..1, wie hart angegriffen wird
# focus = "<tragende Annahme, die geprüft werden soll>"
```

Für den **business**-Modus stattdessen dem Aufbau von `cloud-sme-2027.toml` folgen: `[game]` mit `moves` und mindestens einer `[[game.key_questions]]`, `[business]`, `[[teams]]` (genau ein Markt-Team bei `market_role = "player"`), `[channels]`, `[[injects]]` mit `kind`/`at`.

Effekt-Operationen für Injects: `add` (Track um `by`), `set` (Zustand auf `value`), `fact`, `ongoing`/`stop_ongoing`, `project_advance`, `discover`, `breach`.

## Qualitäts-Checkliste

Vor dem Zeigen des Entwurfs jeden Punkt als ok/offen markieren.

**Frage und Zweck**
- [ ] `purpose` ist eine Frage oder Aufgabe in einem Satz, nicht nur ein Thema.
- [ ] Nach dem Spiel ist klar, welche Beobachtungen die Frage beantworten.

**Akteure**
- [ ] Klassisch: genau vier Fraktionen, eindeutige IDs, `seating.order` enthält genau diese IDs.
- [ ] Ebenen vergleichbar (sonst bewusst begründet).
- [ ] Je Fraktion höchstens vier öffentliche und vier geheime Ziele.
- [ ] Verhalten als Regeln formuliert, nicht als Adjektive; Risiko, Verlustrahmen, Anker, mindestens zwei rote Linien.
- [ ] Keine zwei Fraktionen würden in derselben Lage dasselbe tun.
- [ ] Mindestens ein echter Zielkonflikt zwischen je zwei Fraktionen.

**Lage und Welt**
- [ ] Ausgangslage konkret (Zahlen, Orte, Fristen), keine Allgemeinplätze.
- [ ] Jeder `start` liegt in `[min, max]`; Projekte höchstens drei Stufen, `progress ≤ stages`.
- [ ] Zustände haben eindeutige `values`, `start` ist einer davon.

**Information**
- [ ] Jede Sichtbarkeit verweist auf eine existierende Fraktion.
- [ ] Jede Fraktion weiß etwas, das andere nicht wissen – und umgekehrt.
- [ ] Geheime Ziele sind nicht aus dem öffentlichen Briefing ablesbar.
- [ ] Unterlagen (falls vorhanden) liegen im richtigen Unterordner; Privates nie in `geteilt/`.

**Ende und Auswertung**
- [ ] Jedes Ziel ist an einen Wert, Zustand oder eine Projektstufe gebunden.
- [ ] Rundenzahl passt zur Zeit je Runde und zu den Projekten (Projekt mit drei Stufen braucht genug Runden).

**Schiedsrichter**
- [ ] `max_track_step` und ausgeschlossene Ereignisse sind festgelegt.
- [ ] Höchstens drei Injects, nie zwei in derselben Runde; jede Wirkung referenziert existierende Variablen.
- [ ] Kein Ziel ist nur durch Regelbruch oder Leak erreichbar.

**Form**
- [ ] IDs ohne `:` `,` Leerzeichen `[` `]`.
- [ ] Felder mit Engine-Vorbehalt sind im Bericht genannt.
- [ ] Keine realen Personen oder vertraulichen Daten ohne ausdrückliche Vorgabe.

## Schreiben

- Entwurf zuerst im Chat zeigen.
- Datei nur nach ausdrücklicher Freigabe und nur über die normalen Schreibwerkzeuge (die selbst eine Freigabe verlangen). Vorhandene Dateien nie ohne Bestätigung überschreiben.
- Nach dem Schreiben den Loader bzw. `/matrix` validieren lassen und dessen Meldungen abarbeiten.
