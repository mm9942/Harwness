# Lernschleife: Erkenntnisse erkennen, vorschlagen, nie selbst festschreiben

**Regel:** Was du in einer Sitzung lernst, wird **nur vorgeschlagen**. Du schreibst nie selbst in Memory, in ein Agentenprofil, in ein Skill-Verzeichnis oder in ein Kontextprogramm. Ob etwas dauerhaft gilt, entscheidet der Mensch.

**Warum:** Ein System, das sich selbst umschreibt, landet in Zuständen, die niemand beschlossen hat. Jede weitere „Erkenntnis“ baut dann auf dieser unbeschlossenen Grundlage auf. Ein Vorschlag kostet den Menschen einen Blick; ein falscher Eintrag im Gedächtnis kostet jede folgende Sitzung.

## Wann anwenden

- Am Ende einer längeren Sitzung, in der Entscheidungen gefallen sind oder der Nutzer dich korrigiert hat.
- Sobald der Nutzer sagt: „merk dir …“, „ab jetzt …“, „remember …“, „das nächste Mal …“.
- Wenn du merkst, dass der Nutzer dieselbe Anweisung zum zweiten Mal gibt.
- Wenn der Nutzer einen Ablauf beschreibt, der offensichtlich wiederkehrt („jedes Mal, wenn wir releasen …“).

## Woran du eine dauerhafte Erkenntnis erkennst

Achte vor allem auf die Stimme des Nutzers, nicht auf deine eigenen Antworten:

| Signal | Beispiel | Ziel |
|---|---|---|
| Ausdrücklicher Merkauftrag | „Merk dir: Freitags wird nicht deployt.“ | memory |
| Korrektur | „Nein, die Tests laufen über `make test`, nicht direkt.“ | memory |
| Wiederholte Anweisung | zweimal „Kommentare bitte auf Deutsch“ | memory |
| Entscheidung mit Begründung | „Wir nehmen jiff statt chrono, weil …“ | memory |
| Verhaltensregel für eine Rolle | „Der Reviewer soll zuerst die Tests lesen.“ | agent |
| Wiederkehrender Ablauf | „Jedes Mal vor einem Release: Changelog, Tag, Push.“ | skill |

## Zwei Prüffragen vor jedem Vorschlag

1. **Gilt das in zwei Wochen noch?** Alles, was an „heute“, „gerade“, „vorerst“, einen bestimmten Commit oder einen einzelnen Fehlerfall gebunden ist, fliegt raus.
2. **Steht es nicht schon im Code, in der Git-Historie oder in der Doku?** Was sich aus dem Repository jederzeit wieder ablesen lässt, braucht keinen Gedächtniseintrag.

Dazu eine harte Grenze: **Geheimnisse** (Passwörter, Tokens, Schlüssel) werden nie vorgeschlagen, auch nicht umschrieben.

Im Zweifel: weglassen. Eine verpasste Erkenntnis ist billiger als Rauschen, das jede künftige Sitzung mitschleppt.

## Wie du vorschlägst

- **Formuliere knapp und eigenständig:** ein Satz, der ohne den Gesprächskontext verständlich ist. „Tests laufen über `make test`, nicht über cargo direkt.“ statt „Wie vorhin gesagt …“.
- **Ordne das Ziel zu:** `memory` für Fakten, Regeln und Entscheidungen; `agent` für Verhalten einer bestimmten Rolle; `skill` nur für einen klar abgegrenzten, wiederkehrenden Ablauf mit mehreren Schritten.
- **Lege Vorschläge an, statt zu schreiben:**
  - Der Operator kann mit `/learn` die Sitzung automatisch nach Kandidaten durchsuchen lassen oder mit `/learn note <text> [--target memory|skill|agent]` einen Vorschlag gezielt anlegen. Schlage ihm diese Befehle mit fertig formuliertem Text vor.
  - Für einen Skill kannst du, falls dir das Werkzeug zur Verfügung steht, `skills.propose` benutzen. Das legt nur einen Vorschlag ab; übernommen wird er ausschließlich über `/skills accept <id>`.
  - Rufe niemals `skills.commit_proposal` auf, schreibe keine Memory-Dateien und ändere kein Agentenprofil, nur weil du etwas gelernt hast.
- **Nenne dem Menschen den nächsten Schritt:** Vorschläge prüft er mit `/learn list` und `/learn show <id>`, Memory- und Agenten-Vorschläge entscheidet er mit `/learn accept <id>` bzw. `/learn reject <id> [grund]`, Skill-Vorschläge mit `/skills review|accept|reject <id>`. „Annehmen“ markiert nur; den Eintrag selbst setzt der Operator danach bewusst (z. B. mit `/memory record …`).

## Was du nicht tust

- Keine Selbstbestätigung: Einen eigenen Vorschlag nimmst du nie selbst an.
- Keine Sammelvorschläge: Pro Vorschlag eine Erkenntnis, damit jede einzeln angenommen oder verworfen werden kann.
- Keine Doppelungen: Prüfe mit `/learn list` bzw. `skills.list_proposals`, ob es den Vorschlag schon gibt.
- Keine Spekulation: Schlage nur vor, was der Nutzer tatsächlich gesagt oder entschieden hat, nicht was du vermutest.

## Checkliste am Sitzungsende

- [ ] Nutzerkorrekturen, Merkaufträge, Wiederholungen und Entscheidungen durchgesehen
- [ ] Jede Erkenntnis gegen „in zwei Wochen noch relevant?“ und „nicht aus Code/Git ablesbar?“ geprüft
- [ ] Keine Geheimnisse im Text
- [ ] Ziel (memory, agent, skill) zugeordnet
- [ ] Nur Vorschläge angelegt bzw. dem Operator die passenden `/learn`-Befehle genannt; nichts selbst übernommen
