# Matrix Game — Umpire-geführtes Mehrspieler-Szenario mit Agenten

**Status**: Entwurf  
**Datum**: 2026-09-23  
**Scope**: neues Crate `harw-matrix-game` (deterministischer GameMaster), Szenario-Format `harwness.matrix-scenario/v1`, Matrix-Panel in `harw-tui`  
**Studienmaterial**: J. Curry & T. Price, *Matrix Games for Modern Wargaming* (2014); P. Sabin, *Simulating War* (2012). Alle Inhalte sind in eigenen Worten zusammengefasst; Zitate höchstens ein Satz, mit Quelle.

---

## Inhaltsverzeichnis

1. [Kurzüberblick Matrix-Game-Format](#1-kurzüberblick-matrix-game-format)
2. [Verdeckte Information & private Verhandlung](#2-verdeckte-information--private-verhandlung)
3. [Rundenstruktur als Zustandsmaschine](#3-rundenstruktur-als-zustandsmaschine)
4. [Adjudikation im Harness](#4-adjudikation-im-harness)
5. [Szenario-TOML und JSON-Contracts](#5-szenario-toml-und-json-contracts)
6. [Prompts](#6-prompts)
7. [TUI](#7-tui)
8. [Lernwerte aus beiden Büchern](#8-lernwerte-aus-beiden-büchern)
9. [Testbare Invarianten](#9-testbare-invarianten)
10. [Nicht im ersten Schnitt](#10-nicht-im-ersten-schnitt)
11. [Erweiterungen: Verhalten, Red Cell, Verdacht, Präzedenz, Varianz](#11-erweiterungen-verhalten-red-cell-verdacht-präzedenz-varianz)

---

## Architektur in einem Absatz

Ein **GameMaster** in Rust (`harw-matrix-game`) besitzt den gesamten Spielzustand, das Event-Journal, den RNG und die Routing-Entscheidung, *wer was sieht*. Es gibt kein Agent-zu-Agent-Messaging: Jede Nachricht ist ein Journal-Event mit einer `Audience`, und jeder Prompt an einen Agenten wird ausschließlich aus der **Projektion** des Journals für diesen Sitz gebaut. Vier Spieler-Agenten (`Seat::Player(p)`) und ein Umpire-Agent (`Seat::Umpire`) laufen als **tool-lose Child-Sessions** (Multi-Turn über `ChildController::run_child`, parallel über `run_children`). Der Mensch ist **Facilitator/Beobachter**: Er sieht alles in der TUI und kann eingreifen. LLM-Ausgaben sind die einzige nicht-deterministische Quelle; sie werden journalisiert, sodass ein Replay ohne Modellaufrufe denselben Zustand ergibt.

```
                 ┌────────────────────── harw-matrix-game ──────────────────────┐
 Szenario.toml → │ Scenario ─► GameMaster (Phase-FSM) ─► Journal (append-only)  │
                 │                 │   ▲                    │                    │
                 │   project(seat) │   │ validierte JSON    │ project(Observer)  │
                 └─────────────────┼───┼────────────────────┼────────────────────┘
                                   ▼   │                    ▼
                  run_child(P1..P4, Umpire)             harw-tui Matrix-Panel
                  (tool-lose Child-Sessions)            (Facilitator-Controls)
```

---

## 1. Kurzüberblick Matrix-Game-Format

### 1.1 Kern

Ein Matrix Game erzeugt eine glaubwürdige Erzählung einer Lage, indem Spieler reihum **Argumente** vorbringen: *etwas passiert* plus *Gründe, warum/wie*. Ein Umpire schätzt die Wahrscheinlichkeit, gewürfelt wird nur, wenn Risiko besteht; erfolgreiche Argumente werden Teil der Spielwelt und wirken fort, bis ein anderes Argument sie beendet. Curry & Price bringen das auf die Formel: *"If you can say, 'This happens, for the following reasons...', you can play a Matrix Game."* (Curry & Price, Introduction). Das Format stammt von Chris Engle (1988–1992) und greift die Idee auf, dass These und Gegenthese zu einer Synthese führen.

Wichtig für uns: Es ist **keine Debatte**. Argumente sind kurz und deklarativ, der Umpire entscheidet zügig, damit die Erzählung vorankommt. Genau das passt zu LLM-Agenten — kurze strukturierte Beiträge statt langer Diskussionsschleifen.

### 1.2 Varianten aus Curry & Price und ihre Abbildung

| Variante (Buch) | Inhalt (eigene Worte) | Umsetzung im Harness |
|---|---|---|
| **Three Reasons** | Aktion + genau drei Gründe; Umpire beurteilt nach Plausibilität, Präzedenz, Erfahrung, oft mit Blick auf die Reaktionen der anderen Spieler. Andere Spieler sollen *nicht* dazwischenreden. | `rules.argument_system = "three_reasons"`: Spieler liefert `reasons` (genau 3). Phase *Gegenargumente* entfällt; Umpire bewertet allein. |
| **Pros and Cons** (Empfehlung der Autoren) | Aktion + Pro-Gründe; die anderen Spieler nennen Contra-Gründe; Umpire gewichtet (starke Gründe zählen doppelt, triviale oder wiederholte gar nicht). Nebeneffekt: Die Contras liefern fertige Erklärungen, *warum* ein Argument scheitert. | **Default** `"pros_cons"`. Phase *Gegenargumente*: alle nicht-argumentierenden Spieler liefern Contras parallel. Umpire vergibt Gewichte 0/1/2 je Pro/Contra; Rust rechnet den Zielwert (§4). |
| **Simple Narrative** | Spieler erzählt nur, was als Nächstes passiert; Chance legt Umpire oder ein Mitspieler fest. Sehr zugänglich, aber inkonsistent. | Nicht im ersten Schnitt (Konsistenz leidet, gerade bei LLMs). |
| **Würfel & Basischance** | Pros/Cons ursprünglich 3W6 mit 50 % Basis; die Autoren bevorzugen einen „Erzähl-Bias“: 7+ auf 2W6 (≈58 %), jeder Nettopunkt verschiebt spürbar — das belohnt wenige gute statt vieler schwacher Gründe. | 2W6, Basis 7+, Nettopunkt verschiebt Ziel um 1, Schranken siehe §4.3. |
| **Kein Wurf bei zwingendem Argument** | Ist ein Argument überzeugend, braucht es oft gar keinen Wurf. | `no_roll`-Vorschlag des Umpires nur zulässig, wenn Netto ≥ `rules.auto_success_net` (Default 5) und `rules.allow_auto_success = true`. |
| **Sehr schlechter Wurf scheitert immer** | Nichts ist sicher; ein Scheitern heißt „nicht jetzt“, nicht „nie“. | Pasch 1-1 (natürliche 2) scheitert immer; Zielwert-Obergrenze hält P(Erfolg) ≤ 97,2 %. |
| **Veto gegen triviale/unsinnige Argumente** | Umpire lehnt ab und gibt Gelegenheit zur Neuformulierung. | Umpire-Feld `verdict = "veto"` mit Begründung → GameMaster fragt den Spieler **einmal** nach (Reprompt mit Veto-Grund), danach verfällt der Zug. |
| **Konsens der Runde** | Im Falklands-Beispiel prüft der Umpire einen strittigen Grund, indem er in die Runde fragt; Kopfnicken genügt. | Option `rules.consensus_check = true`: Bei `"disputed": true` für einen Grund werden die Contras der anderen Spieler als Stimmungsbild gezählt (Anzahl Spieler, die den Grund explizit bestreiten). Echte Abstimmungen/Voting-Verfahren beschreibt das Buch nicht — hier bewusst weggelassen. |
| **Offene Begründung** | Umpire legt sein Urteil offen dar, „wie ein Richter bei der Zusammenfassung“. | Umpire liefert pro öffentlichem Argument `public_rationale` (Audience `Public`); Gewichte sind öffentlich. |
| **Geheime Argumente** | Schriftlich an den Umpire; öffentlich wird nur angekündigt, *dass* es ein geheimes Argument gibt. Sparsam einsetzen, nur für konkrete Dinge, die mehrere Züge vorausgeplant sein müssen; ggf. eins pro Spiel. | `secret: true` im Argument; Limit `rules.max_secret_arguments_per_seat` (Default 1); Commitment + Offenlegung §2.4. |
| **Logische Inkonsistenz** | „Es passiert“ vs. „Es passiert nicht“: Das frühere erfolgreiche Argument gilt; Umkehren ist schlechter Stil. Besser subtil darauf aufbauen („der Angriff findet statt, aber schlecht koordiniert“). | Umpire markiert `inconsistent_with`; Rust lässt das in Auflösungsreihenfolge frühere gelten und weist das spätere zurück (mit Hinweis im Seat-Log), sofern der Umpire nicht `conflict` wählt. |
| **Konflikte** | Stehen zwei Seiten in direkter Konfrontation, argumentieren beide über den Ausgang; beide würfeln, bis genau einer Erfolg hat. | `conflicts[]` im Umpire-Output; Rust würfelt gegenüberliegende Paare deterministisch neu bis genau ein Erfolg (Cap 10 Wiederholungen, dann höhere Marge gewinnt). Differenz der Würfe → `margin` für die Erzählung. |
| **Fortwirkende Argumente** | Ein erfolgreiches Argument (Marsch auf X) läuft weiter, bis ein anderes es stoppt. | Effekt-Op `ongoing` → Eintrag in `world.ongoing`, wird in *Rundenende* angewendet, bis ein Effekt `stop_ongoing` ihn entfernt. |
| **Big Projects** | Große Vorhaben brauchen mehrere erfolgreiche Argumente, als Faustregel höchstens drei, sonst dominiert ein Ereignis das Spiel. | `world.projects` mit `stages ≤ 3`; Umpire darf Stufen nur einzeln vorrücken (Rust erzwingt +1 pro Argument). |
| **Versteckte/geschützte Dinge** | Verstecktes muss erst gefunden, Geschütztes Stufe für Stufe überwunden werden. | Szenario-Objekte mit `hidden` und `protection = n`; Effekt-Ops `discover`, `breach` (−1 Stufe). Rust verweigert Aktionen auf noch verdeckte/geschützte Objekte. |
| **Fail Chits** (optional) | Wer scheitert, erhält einen Chip für einen späteren Neuwurf — gegen frühe Pechsträhnen. | `rules.fail_chits = true`; Spieler setzt `use_fail_chit_if_failed` bereits beim Einreichen (keine zusätzliche Runde). |
| **Zugreihenfolge** | Entweder fest pro Szenario oder vom Umpire bestimmt; wer vorne liegt, zieht zuerst, damit Schwächere länger nachdenken können. | `rules.turn_order = "fixed" | "leader_first"`; `leader_first` nutzt die letzte Umpire-Einschätzung `standing`. Bei versiegelter Einreichung (§3) bestimmt die Reihenfolge die **Auflösungspriorität**. |
| **Einheitlicher Zweck** | Jedes Spiel braucht einen knappen, allen klaren Zweck, damit niemand vom Thema abkommt. | Pflichtfeld `scenario.purpose`, erste Zeile jedes Prompts. |
| **Gleiches Rollen-Niveau** | Rollen sollten auf ähnlicher Ebene operieren (nicht drei Generäle und ein einzelner Soldat). | Validierungs-Warnung, wenn `faction.level` nicht einheitlich ist. |
| **Ziele als knappe Stichpunkte** | Briefings werden zu wenigen prägnanten Zielen verdichtet (typisch 3). | `goals.public` / `goals.secret` pro Fraktion, je max. 4 Einträge. |
| **Spielende durch Schlussargumente** | Läuft die Zeit ab, beschreibt jeder Spieler, wie es ausgeht (drei Gründe); alle würfeln gemeinsam, Gescheiterte fliegen raus, bis einer übrig bleibt. | Option `rules.ending = "final_arguments"` (Default `"fixed_rounds"`), eigene Phase `Schlussargumente`. |
| **Hot Wash-Up / AAR** | Nachbesprechung ist der eigentliche Lernertrag; der Umpire wird zum Seminarleiter. Argumente samt Pro/Contra und Umpire-Zusammenfassung mitschreiben; ggf. an strittigen Punkten zurücksetzen und Alternativen durchspielen. | AAR-Dokument am Spielende (§8.2); `fork --from-round N` über das Journal. |

---

## 2. Verdeckte Information & private Verhandlung

### 2.1 Was Sabin dazu sagt (verdichtet)

Sabin unterscheidet **direkte** Simulation des „Nebels des Krieges“ (Spieler sehen tatsächlich weniger — getrennte Räume wie im klassischen Kriegsspiel, verdeckte Steine, Doppelblind-Karten) von **indirekter** Simulation (Zufall und Zugfolge erzeugen Ungewissheit, ohne dass Information asymmetrisch verteilt wird). Seine Warnung: Direkte Informationsasymmetrie macht Spiele komplexer und langsamer, erschwert das Testen (der Designer kann nicht allein spielen) und hindert den Spielleiter daran, Fehler zu korrigieren, ohne Geheimnisse preiszugeben (Sabin, Kap. 7). Umpire-gestützte Mehrspielerspiele sind aber genau der Ort, an dem direkte Verdeckung funktioniert, weil eine neutrale Instanz die Geheimnisse hält.

**Konsequenz für uns**: Wir *wollen* direkte Verdeckung (private Verhandlungen, geheime Ziele, geheime Argumente), aber der GameMaster in Rust übernimmt die Buchhaltung, die Sabin als Last beschreibt, und der Mensch als Facilitator sieht alles — er kann also, wie Sabins „guided competition“, eingreifen, ohne selbst ein Leck zu sein, weil er nicht Mitspieler ist. Zusätzlich nutzen wir Sabins indirekte Mittel: Würfel und versiegelte gleichzeitige Einreichung.

### 2.2 Visibility-Modell

```rust
pub enum Seat { Player(PlayerId), Umpire }

pub enum Audience {
    Public,                          // alle Sitze
    Pair(PlayerId, PlayerId),        // genau diese zwei Spieler (normalisiert: a < b)
    UmpireOnly,                      // nur Umpire
    Seat(PlayerId),                  // genau ein Spieler (z. B. geheime Ziele, eigene Notizen)
    SeatAndUmpire(PlayerId),         // Spieler + Umpire (geheimes Argument, dessen Urteil)
}
// Der Mensch (Observer/Facilitator) ist kein Seat und sieht immer alles.

fn visible_to(aud: &Audience, seat: &Seat, cfg: &VisibilityCfg, ev: &Event) -> bool;
fn project(journal: &Journal, seat: &Seat, cfg: &VisibilityCfg) -> SeatView; // einzige Quelle für Prompts
```

- `SeatView` ist ein Newtype; der Prompt-Builder akzeptiert **nur** `&SeatView`, nie `&Journal` oder `&WorldState`. Damit ist „Spieler kennen nur ihre Projektion“ eine Typ-Eigenschaft.
- Weltvariablen tragen eine eigene Sichtbarkeit (`public`, `umpire`, `seat:<id>`, `seats:[..]`); `project` filtert sie genauso.
- Kanal-IDs für Paare sind opak (`neg-7f3a`), nicht aus Sitznamen ableitbar.

### 2.3 Private Verhandlungen

Anforderung: Unbeteiligte erfahren **weder Inhalt noch Existenz** eines Gesprächs.

1. **Anfrage**: In der Phase *Verhandlung* gibt jeder Spieler (parallel, Audience der Antwort `Seat(p)`) eine Liste von Gesprächswünschen samt Eröffnungsnachricht ab — oder keine.
2. **Öffnung**: Rust öffnet für jedes angefragte Paar einen Kanal (`Audience::Pair`). Der Angefragte sieht die Eröffnung in seinem nächsten Prompt; Ablehnen = nicht antworten oder `decline`.
3. **Austausch**: bis zu `rules.negotiation.max_exchanges` Runden (Default 2). Pro Austausch bekommt jeder Spieler *einen* Aufruf mit all seinen offenen Kanälen; Spieler ohne Kanal werden nicht aufgerufen. Die Aufrufzahl eines Sitzes hängt nur von seinen eigenen Kanälen ab.
4. **Absprachen** (`proposal`/`accept`) werden als `Pair`-Fakt gespeichert, sind aber **nicht bindend** (Rust erzwingt nichts). Verrat ist Spiel.
5. **Öffentlicher Log** enthält nur „Verhandlungsphase beendet“ — ohne Zahl, Dauer oder Teilnehmer.

**Umpire-Zugriff** auf Verhandlungen, Szenario-Option `visibility.umpire_negotiations`:

| Wert | Bedeutung | Einsatz |
|---|---|---|
| `"none"` (**Default**) | Umpire sieht keine Paar-Kanäle. Beruft sich ein Spieler öffentlich auf eine Absprache, ist das seine Offenlegung. | Minimiert Leckfläche: Umpire-Texte werden öffentlich. |
| `"cited"` | Umpire sieht einen Kanal nur, wenn ein Spieler ihn in einem Argument per `cites_negotiation` referenziert — dann als `UmpireOnly`-Kontext für genau diese Adjudikation. | Absprachen als „Gründe“ bewertbar, ohne Dauerzugriff. |
| `"full"` | Umpire sieht alle Kanäle (wie ein Spielleiter, der am Tisch mithört). | Nur mit aktivem Leak-Guard (§2.5). |

Unterscheidung, die überall gilt: **Systemleck** (der Harness gibt etwas weiter, das ein Sitz nicht sehen darf) ist ein Bug. **In-Game-Offenlegung** (Spieler A erzählt C öffentlich oder privat, was B gesagt hat) ist legitimes Spiel und wird im Journal als `disclosed_by: A` markiert.

### 2.4 Geheime Argumente

- **Einreichung**: Argument mit `secret: true`, Audience `SeatAndUmpire(p)`. Rust prüft das Limit (`max_secret_arguments_per_seat`) und die Buch-Regel „nur für Dinge, die vorausgeplant sein müssen“ als Prompt-Vorgabe an den Umpire (der mit `veto` ablehnen darf, wenn es offen hätte vorgebracht werden sollen).
- **Commitment**: Öffentlich erscheint „Spieler *X* bringt ein geheimes Argument vor (#s3)“ plus `sha256(kanonisches JSON ‖ salt)`. Der Text dafür ist ein **Rust-Template**, nie Umpire-Prosa.
- **Keine Gegenargumente** durch Mitspieler (sie kennen den Inhalt nicht); der Umpire liefert selbst die Contras (`umpire_cons`).
- **Wurf**: normal, deterministisch. Öffentlich: nur „#s3 wurde entschieden“ (Ergebnis optional öffentlich via `visibility.secret_outcome_public`, Default `false`).
- **Effekte** dürfen nur Variablen mit Sichtbarkeit ⊆ {`umpire`, `seat:<owner>`} ändern oder müssen als `deferred` markiert sein. Deferred-Effekte werden erst bei Offenlegung angewendet.
- **Offenlegung** (`reveal`): (a) wenn die Bedingung `reveal_when` eintritt (z. B. ein anderes Argument berührt das Objekt; der Umpire meldet `triggers_secret: "s3"`), (b) wenn der Eigentümer es in einem späteren Argument aufdeckt, (c) auf Facilitator-Befehl, (d) **immer** spätestens beim Spielende im AAR. Bei Offenlegung werden Inhalt, Salt und Commitment veröffentlicht — jeder kann den Hash prüfen.

### 2.5 Leak-Guards (Verteidigung in der Tiefe)

1. **Strukturell**: Prompts nur aus `SeatView` (Typsystem, s. o.). Jede Child-Session erhält ausschließlich ihre eigenen Prompts — ihr Transcript kann nie mehr enthalten als ihre Projektion.
2. **Session-Isolation**: Ein Child pro Sitz, keine geteilten Sessions, keine Tools (also auch kein Dateizugriff auf Journal oder Szenario).
3. **Templates für heikle öffentliche Zeilen**: Ankündigungen geheimer Argumente, Phasenwechsel, Verhandlungsende — alles Rust-Text.
4. **Inhaltsscanner** auf allem, was ein Umpire als `Public` ausgibt (und bei `full`/`cited` besonders): Shingle-Überlappung (Wort-5-Gramme) gegen unveröffentlichte `Pair`-, `Seat`- und geheime Inhalte sowie Nennung geheimer Weltvariablen-Namen/Werte. Treffer → ein Reprompt mit Hinweis; zweiter Treffer → Text wird zurückgehalten, Facilitator bekommt `LeakSuspect` zur Freigabe/Bearbeitung.
5. **Spieler-Scanner** nur als *Kennzeichnung*: Zitiert ein Spieler öffentlich Inhalte eines eigenen Paar-Kanals, wird das als In-Game-Offenlegung markiert, nicht blockiert. Zitiert er Inhalte, die er gar nicht kennen kann, ist das ein Systemleck-Alarm (Test-Invariante).
6. **Kein Metadaten-Leck**: Öffentliche Events tragen keine Zeitstempel oder Sequenznummern aus der Verhandlungsphase; Rundenlog-Nummerierung ist pro Audience lokal.

---

## 3. Rundenstruktur als Zustandsmaschine

```
Setup ─► Briefing ─► Verhandlung ─► Argumente ─► Gegenargumente ─► Adjudikation ─► Veröffentlichung ─► Rundenende
            ▲                                                                                          │
            └────────────────────────────── nächste Runde (Lagebild) ◄─────────────────────────────────┤
                                                                                                        ▼
                                                                   [Schlussargumente] ─► Spielende/AAR
```

Jeder Übergang ist ein Journal-Event `PhaseEntered{round, phase}`. Der GameMaster führt pro Phase eine feste Aufrufmenge aus; Facilitator-Befehle (§7.2) werden nur an Phasengrenzen angewendet (außer `pause`).

| Phase | Wer handelt | Input (Projektion) | Output (validiert) | Journal-Effekt / Audience |
|---|---|---|---|---|
| **Setup** | Rust | Szenario-TOML, Seed | `WorldState₀`, Sitzzuordnung, Child-Sessions | `GameCreated` (Observer), Commitment auf Szenario-Hash |
| **Briefing** (Runde 1: voll; danach „Lagebild“) | alle 4 Spieler + Umpire (parallel) | Spieler: Zweck, öffentl. Lage, eigene Fraktion, öffentl. + **eigene** geheime Ziele, eigene geheime Variablen. Umpire: alles außer Paar-Kanälen (je nach Option) | Spieler: `ack` + kurze private Absicht (`intent`, Audience `Seat(p)`) — dient nur der AAR | `Briefed{seat}` |
| **Verhandlung** | Spieler mit Wunsch/Kanal | eigene Kanäle + Lage | `negotiation_request[]`, dann `negotiation_message[]` je Austausch | `Pair`-Events; öffentlich nur `NegotiationClosed` |
| **Argumente** | alle 4 Spieler, **versiegelt, parallel** | Lage, eigene Kanäle, laufende Effekte, eigene Fail-Chits, Secret-Kontingent | `player_argument` | Sofort `ArgumentSealed{id, commitment}`; Inhalt erst nach Eingang *aller* vier → `ArgumentsRevealed` (`Public` bzw. `SeatAndUmpire`) |
| **Gegenargumente** (nur `pros_cons`) | je Spieler einmal für alle fremden öffentlichen Argumente (parallel) | alle öffentlichen Argumente der Runde | `counter_argument` (Liste Contras je Argument-ID) | `CountersSubmitted` `Public` |
| **Adjudikation** | Umpire (2 Aufrufe) + Rust | Aufruf A: alle Argumente, Contras, Lage, ggf. Kanäle. Aufruf B: Würfelergebnisse | A: `umpire_adjudication` (Gewichte, Modifikator, Effekte *für Erfolg und Misserfolg*, Konflikte, Vetos). Rust: Validierung → Zielwerte → Würfe. B: `umpire_narration` | `Adjudicated`, `DiceRolled` (Public für öffentliche Args), `Narrated` |
| **Veröffentlichung** | Rust | Ergebnis-Zweige | angewandte Deltas | `WorldDelta` je Variable mit deren Sichtbarkeit; `PublicSummary` |
| **Rundenende** | Rust (+ Umpire optional `standing`) | Weltzustand | `ongoing` anwenden, Offenlegungs-Trigger prüfen, Endbedingung prüfen, Snapshot | `RoundClosed{round, state_hash}` |
| **Schlussargumente** (optional) | alle Spieler, dann Umpire, dann Rust | Gesamtlage | je Spieler ein Ausgangs-Argument (3 Gründe); Umpire → Zielwerte; Rust → Knock-out-Würfe | `FinalOutcome` Public |
| **Spielende/AAR** | Umpire + optional Spieler (Debrief) | **volle Offenlegung** (alle Audiences werden Public) | `aar_input` je Sitz, Umpire-Synthese | AAR-Markdown (§8.2) |

**Warum versiegelt statt reihum?** Curry & Price spielen Argumente nacheinander. Sabin zeigt aber, dass bei Verfahren, in denen eine Seite erst nach der anderen entscheidet, der Später-Ziehende einen erheblichen Informationsvorteil hat (Kap. 7). Versiegelte Einreichung beseitigt das, erlaubt parallele LLM-Aufrufe und erhält trotzdem die Reihenfolge als Auflösungspriorität für Inkonsistenzen. Option `rules.argument_mode = "sequential"` bleibt für Szenarien, in denen Reaktion auf das Vorargument gewollt ist (dann sieht Spieler *n* die veröffentlichten Argumente 1..n−1 dieser Runde).

**Rundenzahl**: Default 6, Szenario 6–8 empfohlen — Curry & Price berichten, dass weniger als sechs Züge Themen nicht reifen lassen und zu viele repetitiv werden (Lasgah Pol). Optional „Vor-Runde“ (Pre-Deployment) und „Nach-Runde“ (was passiert danach) als `rules.prologue`/`rules.epilogue`.

**Fehlerpfade**: Ungültiges JSON → ein Reprompt mit Parser-Fehler; zweites Scheitern → Sitz „passt“ in dieser Phase (`Forfeit{seat, phase}` Public als „Spieler X bringt kein Argument vor“). Umpire-Fehler → Phase pausiert, Facilitator entscheidet (Retry / manuelle Adjudikation).

---

## 4. Adjudikation im Harness

### 4.1 Arbeitsteilung

| Schritt | Wer | Warum |
|---|---|---|
| Gründe bewerten, Kontext-Modifikator, Inkonsistenzen, Konflikte, Vetos | **Umpire (LLM)** | Urteilskraft — das ist der Kern des Formats. |
| Effekte für **Erfolg und Misserfolg vorab** festlegen | **Umpire (LLM)** | Verhindert, dass die Folgen nach dem Wurf an die Erzählung angepasst werden. |
| Gewichte → Zielwert → Wahrscheinlichkeit | **Rust** | Einheitliche, prüfbare Abbildung; Umpire kann keine beliebigen Zahlen setzen. |
| Würfeln | **Rust**, seeded, journalisiert | Determinismus, Replay. |
| Delta anwenden, Schranken erzwingen | **Rust** | Weltzustand ist nie LLM-geschrieben. |
| Erzählung des Ergebnisses | **Umpire (LLM)**, Aufruf B | Nur Prosa, keine Zustandsänderung möglich. |

### 4.2 Gewichtungsschema (Default `pros_cons_2d6`)

- Jeder Pro-Grund und jeder Contra-Grund erhält vom Umpire ein Gewicht `w ∈ {0, 1, 2}`:
  `0` = trivial, bloße Umformulierung eines schon gezählten Grundes, oder faktisch falsch; `1` = tragfähig; `2` = zwingend.
- Es zählen höchstens die **drei höchstgewichteten Pros und drei Contras** (gegen „Wäschelisten“).
- Kontext-Modifikator `m ∈ [−2, +2]` mit Pflichtbegründung (inhärente Wahrscheinlichkeit, Lage, Präzedenz — z. B. im Falklands-Beispiel ein völkerrechtlicher Punkt, den keiner der Spieler genannt hatte).
- `net = Σ top3(pro) − Σ top3(con) + m`, also `net ∈ [−8, +8]`.
- `target = clamp(7 − net, 3, 11)`; Erfolg, wenn `2W6 ≥ target` **und** Wurf ≠ 2 (Pasch 1-1 scheitert immer).
- Fail-Chit (falls gesetzt und vorhanden): genau ein Neuwurf, zweiter Wurf zählt.

| target | 3 | 4 | 5 | 6 | **7** | 8 | 9 | 10 | 11 |
|---|---|---|---|---|---|---|---|---|---|
| P(Erfolg) | 97,2 % | 91,7 % | 83,3 % | 72,2 % | **58,3 %** | 41,7 % | 27,8 % | 16,7 % | 8,3 % |

Schranken: P(Erfolg) ∈ [8,3 %; 97,2 %]. Nichts ist unmöglich, nichts sicher — außer der Umpire schlägt `no_roll` vor und `net ≥ auto_success_net` (Default 5) bei `allow_auto_success = true`.

**Alternative** `rules.adjudication = "estimative_d100"`: Umpire wählt eine Stufe aus einer festen Leiter (`5, 15, 30, 50, 70, 85, 95` %, verbal: „nahezu ausgeschlossen“ … „nahezu sicher“) plus Begründung; Rust akzeptiert nur Leiterwerte und würfelt W100. Diese Variante steht nicht bei Curry & Price, ist aber für Szenarien mit Analysten-Zielgruppe nützlich; Default bleibt 2W6, weil die Nettopunkt-Logik die Pro/Contra-Struktur transparent macht.

**Konflikte**: Für ein Konfliktpaar (A, B) wird jeweils `target_A`, `target_B` wie oben bestimmt; Rust würfelt beide, wiederholt bis genau einer Erfolg hat (Cap 10, danach gewinnt die größere Marge `roll − target`, bei Gleichstand der in Auflösungsreihenfolge frühere). `margin = |Differenz|` geht an die Erzählung („knapper Sieg, geordneter Rückzug möglich“ vs. „Zusammenbruch“).

**Nachträgliche Grade**: Wurf ≥ target+3 oder ≤ target−3 wird als `strong_success`/`strong_failure` an die Erzählung gemeldet. Zustandswirkung bleibt der vorab festgelegte Zweig (keine Zusatzeffekte — Einfachheit vor Nuance).

### 4.3 Deterministischer RNG

- `master_seed: [u8; 32]` aus Szenario (`seed`) oder beim Start zufällig gezogen und **im Journal als erstes Event** gespeichert.
- Jeder Wurf nutzt einen **abgeleiteten** Seed, unabhängig von der Aufrufreihenfolge:
  `sub_seed = SHA-256("harw-matrix/v1" ‖ master_seed ‖ round ‖ roll_kind ‖ arg_id ‖ attempt)` → `ChaCha20Rng::from_seed(sub_seed)` → zwei Würfe `gen_range(1..=6)`.
  Damit ändern parallele Ausführung, Retries oder ein eingefügter Facilitator-Eingriff die übrigen Würfe nicht.
- `DiceRolled{arg_id, attempt, dice:[u8;2], target, success, sub_seed_hex}` wird journalisiert; Replay prüft, dass Neuberechnung identische Werte liefert (sonst `ReplayDivergence`).

### 4.4 Weltzustand & Delta

Effekt-Operationen (vom Umpire vorgeschlagen, von Rust validiert):

| Op | Wirkung | Rust-Prüfung |
|---|---|---|
| `add {var, by}` | Track verschieben | Var existiert; `|by| ≤ rules.max_track_step` (Default 1, Szenario bis 2); Clamp auf `[min,max]` |
| `set {var, value}` | Diskreten Zustand setzen | Wert in `values` erlaubt |
| `fact {text, audience}` | Erzählfakt in `world.facts` | Audience ⊆ Audience des Arguments |
| `ongoing {id, text, each_round:[ops]}` | fortwirkender Effekt | ops selbst gültig; max. `rules.max_ongoing` (Default 6) |
| `stop_ongoing {id}` | beendet ihn | id existiert |
| `project_advance {id}` | Big Project +1 Stufe | max. +1 pro Argument, `stages ≤ 3` |
| `discover {object}` / `breach {object}` | versteckt → bekannt / Schutz −1 | Reihenfolge: erst `discover`, dann `breach` |
| `reveal_secret {secret_id}` | Offenlegung auslösen | Secret existiert, noch nicht offen |

Verletzungen führen zu einem Reprompt des Umpires mit der Fehlerliste (ein Versuch), danach werden ungültige Ops verworfen und als `EffectRejected` für den Facilitator sichtbar. Der Umpire schreibt nie direkt in den Zustand; `state_hash` (SHA-256 über kanonisches JSON) wird an jedem Rundenende journalisiert.

---

## 5. Szenario-TOML und JSON-Contracts

### 5.1 Vollständiges Beispielszenario (fiktiv)

```toml
schema = "harwness.matrix-scenario/v1"
id = "karst-wasserkrise"
title = "Wasserkrise auf den Karst-Inseln"
purpose = "Ein Matrix Game über den Streit um die einzige Entsalzungsanlage der Karst-Inseln in einem Dürresommer."
seed = "optional-hex-oder-leer"          # leer = beim Start ziehen und journalisieren
rounds = 6
round_represents = "etwa zwei Wochen"

[rules]
argument_system = "pros_cons"            # "pros_cons" | "three_reasons"
argument_mode = "simultaneous"           # "simultaneous" (versiegelt) | "sequential"
adjudication = "pros_cons_2d6"           # "pros_cons_2d6" | "estimative_d100"
turn_order = "fixed"                     # "fixed" | "leader_first"
max_track_step = 1
max_ongoing = 6
allow_auto_success = true
auto_success_net = 5
fail_chits = true
consensus_check = false
max_secret_arguments_per_seat = 1
ending = "fixed_rounds"                  # "fixed_rounds" | "final_arguments"
prologue = false
epilogue = true
debrief_players = true                   # Spieler-Agenten liefern AAR-Reflexion

[rules.negotiation]
enabled = true
max_exchanges = 2
max_channels_per_seat = 2
max_message_chars = 800

[visibility]
umpire_negotiations = "none"             # "none" | "cited" | "full"
secret_outcome_public = false
leak_guard = "strict"                    # "strict" | "flag_only"

[models]                                 # optional, sonst Default-Modell des Harness
umpire = "default"
players = "default"

# ---------- Welt ----------
[world]
public_situation = """
Seit sechs Wochen kein Regen. Die Entsalzungsanlage im Hafen von Velmar liefert nur noch
60 % Leistung. Der Inselrat hat Rationierung angekündigt; die Hafengilde kontrolliert
den Tankerverkehr, das Nordreich bietet „technische Hilfe“ an, eine Vermittlungsmission
der Seeliga ist seit gestern vor Ort.
"""

tracks = [
  { id = "stability",           label = "Öffentliche Ordnung",          min = -3, max = 3, start = 0,  visibility = "public" },
  { id = "water",               label = "Wasserversorgung",             min = -3, max = 3, start = -1, visibility = "public" },
  { id = "north_influence",     label = "Einfluss Nordreich",           min = 0,  max = 5, start = 1,  visibility = "public" },
  { id = "rat_support",         label = "Rückhalt des Inselrats",       min = -3, max = 3, start = 1,  visibility = "public" },
  { id = "smuggling_net",       label = "Schmuggelnetz der Gilde",      min = 0,  max = 3, start = 2,  visibility = "seat:gilde" },
  { id = "north_agents",        label = "Nordreich-Agenten im Hafen",   min = 0,  max = 3, start = 1,  visibility = "seat:nord" },
  { id = "plant_sabotage_risk", label = "Sabotagerisiko Anlage",        min = 0,  max = 3, start = 1,  visibility = "umpire" },
]
states = [
  { id = "plant_control", label = "Kontrolle Entsalzungsanlage", values = ["rat", "gilde", "nord", "mission", "umstritten"], start = "rat", visibility = "public" },
]
objects = [
  # nur der Umpire kennt das Reservoir zu Beginn; verdeckt, 1 Schutzstufe
  { id = "reservoir_altmark", label = "Altes Reservoir von Altmark", hidden = true, protection = 1, visibility_when_found = "public" },
]
projects = [
  { id = "pipeline", label = "Notleitung vom Festland", stages = 3, progress = 0, visibility = "public" },
]

# ---------- Fraktionen (genau 4 Spieler-Sitze) ----------
[[factions]]
id = "rat"
name = "Inselrat"
level = "Regierung"
briefing = "Gewählte Regierung, knappe Kasse, Wahlen in drei Monaten."
goals.public = ["Öffentliche Ordnung nicht unter 0 fallen lassen", "Kontrolle über die Anlage behalten"]
goals.secret = ["Die Gilde als Schuldige der Krise dastehen lassen"]
assets = ["Inselpolizei", "Rationierungsbehörde"]

[[factions]]
id = "gilde"
name = "Hafengilde"
level = "Wirtschaftsmacht"
briefing = "Kontrolliert Tanker, Kräne und die Hafenarbeiter."
goals.public = ["Tankerlizenzen sichern", "Rationierung zu Gunsten des Hafens"]
goals.secret = ["Schmuggelnetz ausbauen (smuggling_net = 3)", "Kontrolle über die Anlage erlangen"]
assets = ["Tankerflotte", "Hafenarbeiter-Syndikat"]

[[factions]]
id = "nord"
name = "Nordreich"
level = "Nachbarstaat"
briefing = "Großer Nachbar mit Überschusswasser und politischen Ambitionen."
goals.public = ["Als Helfer in der Not wahrgenommen werden"]
goals.secret = ["north_influence mindestens 4 bei Spielende", "Pipeline-Projekt verhindern"]
assets = ["Tankschiffe", "Techniker", "Botschaft"]

[[factions]]
id = "mission"
name = "Vermittlungsmission der Seeliga"
level = "Internationale Organisation"
briefing = "Kleines Team mit Mandat, aber ohne Machtmittel."
goals.public = ["Eskalation verhindern", "Pipeline zu mindestens 2 Stufen bringen"]
goals.secret = ["Die eigene Organisation als unverzichtbar positionieren"]
assets = ["Mandat", "Geberkonferenz-Kontakte"]

[seating]                                  # Sitz → Fraktion, Reihenfolge = Zugreihenfolge
order = ["rat", "gilde", "nord", "mission"]

[[injects]]                                # vorbereitete Facilitator-Ereignisse (optional)
id = "hitzewelle"
round = 3
audience = "public"
text = "Eine Hitzewelle verdoppelt den Wasserbedarf."
effects = [{ op = "add", var = "water", by = -1 }]
```


**Validierung beim Laden**: genau 4 Fraktionen; eindeutige IDs; `start ∈ [min,max]`; Sichtbarkeiten verweisen auf existierende Fraktionen; `stages ≤ 3`; `purpose` nicht leer; Warnung bei uneinheitlichem `level`; max. 4 Ziele je Kategorie.

### 5.2 JSON-Contracts

Alle Antworten sind **ein** JSON-Objekt, validiert per Serde (`deny_unknown_fields`) plus Längen-Limits. Der GameMaster setzt IDs; Agenten referenzieren nur IDs, die in ihrer Projektion vorkommen (sonst Validierungsfehler).

**negotiation_request** (Spieler, Phase Verhandlung, Schritt 1)
```json
{ "requests": [ { "to": "nord", "opening": "Wir sollten über Tankerlizenzen reden, bevor der Rat es tut." } ] }
```

**negotiation_message** (Spieler, je Austausch; ein Eintrag pro offenem Kanal)
```json
{
  "messages": [
    { "channel": "neg-7f3a", "text": "Gegen 2 Tanker Wasser pro Woche unterstützen wir eure Hilfszusage öffentlich.",
      "proposal": { "summary": "2 Tanker/Woche gegen öffentliche Unterstützung" }, "accept": null, "decline": false }
  ]
}
```

**player_argument**
```json
{
  "action": "Die Gilde übernimmt den Betrieb der Entsalzungsanlage im Auftrag des Rates.",
  "pros": ["Die Gilde stellt die einzigen Techniker mit Hafenkran-Zugang.",
           "Der Rat hat kein Budget für Überstunden.",
           "Die Hafenarbeiter streiken sonst."],
  "secret": false,
  "cites_negotiation": [],
  "conflict_target": null,
  "project": null,
  "use_fail_chit_if_failed": false,
  "private_note": "Erster Schritt zur Kontrolle der Anlage."
}
```
`private_note` hat Audience `Seat(p)` (nur für AAR und Facilitator). Bei `three_reasons` müssen genau 3 `pros` vorliegen, sonst 1–5.

**counter_argument**
```json
{
  "counters": [
    { "argument_id": "r2-a1", "cons": ["Der Rat würde seine wichtigste Machtbasis aus der Hand geben.",
                                         "Die Polizei bewacht die Anlage bereits."] },
    { "argument_id": "r2-a3", "cons": [] }
  ]
}
```
Nur Argument-IDs fremder, öffentlicher Argumente der laufenden Runde; max. 3 Contras je Argument.

**umpire_adjudication** (Aufruf A, vor dem Wurf)
```json
{
  "rulings": [
    {
      "argument_id": "r2-a1",
      "verdict": "roll",
      "pro_weights": [2, 1, 1],
      "con_weights": { "rat": [2], "mission": [1] },
      "umpire_cons": [],
      "context_modifier": -1,
      "context_reason": "Eine Regierung gibt kritische Infrastruktur selten freiwillig ab.",
      "inconsistent_with": null,
      "public_rationale": "Technisch plausibel, politisch für den Rat teuer; die Streikdrohung wiegt schwer.",
      "private_notes": "Wenn erfolgreich, steigt Sabotagerisiko intern nicht.",
      "on_success": [ { "op": "set", "var": "plant_control", "value": "gilde" },
                      { "op": "add", "var": "rat_support", "by": -1 } ],
      "on_failure": [ { "op": "fact", "text": "Der Rat lehnt ab; die Gilde droht offen mit Streik.", "audience": "public" } ],
      "triggers_secret": null
    }
  ],
  "conflicts": [],
  "standing": ["gilde", "rat", "nord", "mission"]
}
```
`verdict ∈ {"roll", "no_roll", "veto"}`; `veto` erfordert `public_rationale` als Ablehnungsgrund. `con_weights` ist nach Fraktion gruppiert und in der Reihenfolge der eingereichten Contras. Für geheime Argumente: `umpire_cons` statt `con_weights`, `public_rationale` muss `null` sein (Rust-Template ersetzt es).

**umpire_narration** (Aufruf B, nach dem Wurf)
```json
{
  "narrations": [
    { "argument_id": "r2-a1", "audience": "public",
      "text": "Nach zähen Gesprächen übergibt der Rat den Betrieb; Gildentechniker ziehen in die Anlage ein." }
  ],
  "round_summary": "Die Gilde hat den Wasserhahn in der Hand; der Rat verliert an Rückhalt."
}
```
`audience` einer Erzählung darf nicht weiter sein als die des Arguments; Leak-Guard läuft über alle `public`-Texte.

---

## 6. Prompts

Outlines, nicht Endtexte. Alle Prompts sind deutsch oder in `scenario.language`; die JSON-Schemata werden wörtlich angehängt.

### 6.1 System-Prompt Spieler

1. **Rolle**: „Du spielst die Fraktion *{name}* in einem Matrix Game. Zweck: *{purpose}*.“ Du bist Akteur, nicht Erzähler oder Schiedsrichter.
2. **Was du weißt**: „Du kennst ausschließlich, was in deinem Lagebild steht. Es gibt Dinge, die du nicht weißt — andere Fraktionen haben eigene Ziele und können privat miteinander sprechen, ohne dass du es erfährst. Erfinde kein Wissen über fremde geheime Ziele, Absprachen oder verdeckte Werte.“
3. **Argumentieren**: eine konkrete Aktion für diesen Zug, auf Ebene deiner Fraktion; wenige starke Gründe statt vieler schwacher; nichts, was ein bereits erfolgreiches Ereignis einfach umkehrt — besser darauf aufbauen. Große Vorhaben in Schritten.
4. **Gegenargumente**: sachliche Gründe, warum ein fremdes Argument scheitern könnte; keine Wiederholung, keine Polemik.
5. **Verhandeln**: Absprachen sind nicht bindend; du darfst bluffen und Absprachen brechen. Was in einem privaten Kanal steht, bleibt privat, solange *du* es nicht bewusst offenlegst — tust du es, ist das ein Spielzug.
6. **Geheime Argumente**: höchstens {n} pro Spiel, nur für konkrete Vorbereitungen, die über mehrere Züge verborgen bleiben müssen.
7. **Form**: Antworte ausschließlich mit einem JSON-Objekt nach Schema; keine Metakommentare über das Spiel, das Modell oder den Harness.
8. **Anti-Leak (Selbstschutz)**: „Füge Inhalte eines privaten Kanals nur dann in öffentliche Felder ein, wenn du sie absichtlich offenlegen willst; markiere das in `private_note`.“

### 6.2 System-Prompt Umpire

1. **Rolle**: neutraler Schiedsrichter und Erzähler. Ziel ist eine glaubwürdige, zusammenhängende Erzählung, nicht ein Sieger. Keine Fraktion bevorzugen.
2. **Bewerten**: Jeden Grund auf Plausibilität, Lage, Präzedenz prüfen; Gewichte 0/1/2 nach festem Maßstab; Kontext-Modifikator nur mit Begründung und nur für Faktoren, die kein Spieler genannt hat. Nicht vorwegnehmen, was Würfel entscheiden sollen; keine Würfel als Ersatz für fehlendes Urteil (Sabin nach Rubel).
3. **Vetos**: triviale, unrealistische oder spielbrechende Argumente („Ich greife an und gewinne den Krieg“) zurückweisen oder als riskant bewerten — Curry & Price schildern genau diesen Fall.
4. **Effekte vor dem Wurf** für Erfolg *und* Misserfolg festlegen, klein und innerhalb der Schranken; Misserfolge aus den Contras erklären.
5. **Konsistenz**: auf frühere erfolgreiche Argumente, laufende Effekte und Big Projects achten; Inkonsistenzen melden statt still zu übergehen.
6. **Geheimhaltung**: „Was du als UmpireOnly oder aus geheimen Argumenten weißt, darf in öffentlichen Texten weder zitiert noch angedeutet werden. Öffentliche Begründungen stützen sich nur auf öffentlich Bekanntes. Private Verhandlungen (falls sichtbar) nie erwähnen; nicht einmal, dass sie stattfanden.“
7. **Erzählen** (Aufruf B): kurz, konkret, Ergebnis gemäß Würfelgrad (`strong_success` etc.), keine neuen Zustandsänderungen.
8. **AAR-Modus** (Spielende): Rolle wechselt zum Seminarleiter — Muster, Schlüsselmomente, Alternativen, Bezug zum Zweck.

### 6.3 User-Prompt je Phase (Aufbau)

```
[Zweck] … [Runde r/R, Phase] …
[Öffentliche Lage]    ← project(seat).public_world
[Deine Fraktion]      ← Briefing, Ziele (öffentlich + eigene geheime), eigene verdeckte Tracks
[Laufende Effekte / Big Projects / bekannte Objekte]
[Öffentliches Protokoll seit deinem letzten Zug]   ← Delta, nicht voller Verlauf (Session ist multi-turn)
[Deine privaten Kanäle] (nur eigene)
[Auftrag dieser Phase] + JSON-Schema
```

Weil die Child-Session multi-turn ist, wird pro Aufruf nur das **Delta** der Projektion gesendet; zu Rundenbeginn zusätzlich ein vollständiges „Lagebild“ (Schutz gegen Kontextdrift und Grundlage für Kompaktierung).

---

## 7. TUI

### 7.1 Matrix-Panel

```
┌ Matrix: Wasserkrise auf den Karst-Inseln ─ Runde 3/6 ─ Phase: ADJUDIKATION ─ Seed 9f2c… ─ [AUTO 2] ┐
│ WELT                         │ ÖFFENTLICHES PROTOKOLL                                            │
│ Öffentl. Ordnung   ▓▓▓░░░ -1 │ r3 Gilde: „übernimmt Betrieb der Anlage“ Pro 3 / Contra 2         │
│ Wasserversorgung   ▓▓░░░░ -2 │    Umpire: net +1 (2+1+1 −2−1 −1) → Ziel 6+ (72 %)                │
│ Einfluss Nordreich ▓▓▓░░   2 │    🎲 4+5 = 9  ✔ Erfolg   → plant_control = gilde, rat_support −1 │
│ Anlage: gilde                │ r3 Nordreich bringt ein geheimes Argument vor (#s1) [sha 51e0…]   │
│ Pipeline: ■□□ (1/3)          │ r3 Inject (Facilitator): Hitzewelle — Wasser −1                   │
│ ── verdeckt ──               │                                                                    │
│ [gilde] Schmuggelnetz 2      │                                                                    │
│ [nord]  Agenten 1            │                                                                    │
│ [umpire] Sabotagerisiko 1    │                                                                    │
├──────────────────────────────┴────────────────────────────────────────────────────────────────────┤
│ Tabs: [Öffentlich] [Umpire] [rat⇄nord 🔒] [gilde⇄nord 🔒] [Geheim: #s1 🔒] [Sitz: gilde] [Journal]│
│ ── rat⇄nord · nur für rat & nord sichtbar — andere Spieler wissen nicht, dass es diesen Kanal gibt│
│  rat : „Wir brauchen Tankschiffe, aber ohne Fahnen.“                                              │
│  nord: „Gegen Landerecht am Nordkai.“   ⟶ Vorschlag offen                                          │
├───────────────────────────────────────────────────────────────────────────────────────────────────┤
│ [s] Schritt  [a] Auto N  [p] Pause  [i] Inject  [o] Override  [v] Veto  [r] Reveal  [f] Fork  [?] │
└───────────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Welt**: alle Tracks, States, Objekte, Projekte; verdeckte Werte mit Besitzer-Präfix, farblich abgesetzt.
- **Kopfzeile**: Runde/Phase, Seed-Präfix, Auto-Zähler, laufende Child-Aufrufe (Spinner pro Sitz).
- **Öffentliches Protokoll**: exakt das, was `project(Public)` enthält — so sieht der Mensch, was Spieler sehen.
- **Tabs pro Paar-Kanal** mit Schloss und der festen Kennzeichnung **„nur für X & Y sichtbar“**; Tab „Sitz: …“ zeigt die komplette Projektion eines Spielers („Was weiß die Gilde gerade?“) — das wichtigste Debug-Werkzeug gegen Leaks.
- **Umpire-Tab**: Gewichte je Grund, Kontext-Modifikator + Begründung, `private_notes`, vorab festgelegte Erfolg/Misserfolg-Effekte, Leak-Guard-Befunde.
- **Würfel**: jeder Wurf mit Ziel, Wahrscheinlichkeit, Augen, Fail-Chit-Einsatz, `sub_seed`-Präfix.
- **Journal-Tab**: rohe Events mit Audience-Spalte.

### 7.2 Facilitator-Controls

| Taste | Befehl | Wirkung | Journal-Event |
|---|---|---|---|
| `s` | Schritt | eine Phase weiter | `FacilitatorStep` |
| `a` | Auto N | N Runden ohne Halt (bricht bei Fehler, `LeakSuspect`, Veto-Reprompt-Scheitern ab) | `FacilitatorAuto{n}` |
| `p` | Pause | nach laufenden Aufrufen anhalten | `FacilitatorPause` |
| `i` | Inject | Ereignis mit Text, Audience (Public/Seat/Pair/UmpireOnly) und validierten Effekt-Ops; wird an der nächsten Phasengrenze wirksam, öffentlich als „Ereignis“ ohne Urheber, außer `attributed = true` | `Inject{…}` |
| `o` | Override | **vor dem Wurf**: Gewichte/Modifikator/Ziel ändern; **nach dem Wurf**: Ergebnis kippen oder Effekte ersetzen (deutlich markiert) | `AdjudicationOverride{before_roll: bool, …}` |
| `v` | Veto | Argument verwerfen (Spieler bekommt einen Neuversuch) | `FacilitatorVeto` |
| `r` | Reveal | geheimes Argument oder verdeckten Track offenlegen | `SecretRevealed{by: Facilitator}` |
| `f` | Fork | neues Spiel ab Rundenende N desselben Journals (Curry & Price: an strittigen Stellen zurücksetzen und Alternativen durchspielen) | `ForkedFrom{game, round}` |
| `e` | Ende | direkt zu Schlussargumenten/AAR | `FacilitatorEnd` |

Overrides sind Teil des Journals und damit replaybar; die Würfel anderer Argumente ändern sich dadurch nicht (abgeleitete Seeds, §4.3).

---

## 8. Lernwerte aus beiden Büchern

### 8.1 Was wir kodieren

| Lernwert | Quelle | Kodierung |
|---|---|---|
| Einfach halten; ein gespieltes einfaches Spiel lehrt mehr als ein detailliertes, das nie gespielt wird. Sabin: *"a simple wargame that is played will be more instructive than a detailed wargame that is not."* | Sabin, Kap. 2 | Wenige Tracks (Validierungs-Warnung > 10), max. 4 Ziele, Effektschritt ±1, keine Kampftabellen im ersten Schnitt. |
| Freies Kriegsspiel steht und fällt mit einem angesehenen, kundigen, unparteiischen Umpire; darum entwickelte sich eine Mischform mit Tabellen. | Sabin, Kap. 3 | Hybrid: LLM urteilt, Rust erzwingt Gewichtsskala, Schranken, Würfel. |
| Würfel sind kein Ersatz für nicht modellierte Realität. | Sabin, Kap. 8 (Rubel) | Umpire muss Kontext explizit als Modifikator begründen; `no_roll` für klare Fälle. |
| Realität, Können, Zufall als Dreieck; Zufall bricht Rückschau-Wissen, darf aber gute Entscheidungen nicht überwiegen. | Sabin, Kap. 7–8 | 2W6 statt W6 (glockenförmig), Zielwert-Schranken, Fail-Chits als Ausgleich. |
| Siegbedingungen steuern Verhalten ebenso stark wie Bewegungs- und Kampfregeln. | Sabin, Kap. 8 | Öffentliche + geheime Ziele, im AAR bewertet (0–3 je Ziel mit Begründung); kein globaler Sieger per Default. |
| Mehrspieler mit eigenen Zielen erzeugt Kooperation jenseits von Nullsumme (Diplomacy-artig); kurze Verhandlungsfenster vor den Entscheidungen, dann Ansagen in fester Reihenfolge. | Sabin, Kap. 7/9 | Verhandlungsphase vor Argumenten, begrenzte Austauschrunden, feste Auflösungsreihenfolge. |
| Nebel des Krieges direkt kostet Komplexität und Testbarkeit. | Sabin, Kap. 7 | Buchhaltung in Rust; Projektionen per Property-Test; Mensch sieht alles. |
| Designs sind nie fertig, nur aufgegeben (Vasey via Sabin); wiederholt spielen, um die Streuung zu sehen. | Sabin, Kap. 8 | Batch-Modus `--runs N` mit unterschiedlichen Seeds, Auswertung Endzustände (später). |
| Validierung: Ergibt historisches Verhalten ungefähr den bekannten Verlauf? Wählen rationale Spieler manchmal die realen Strategien? | Sabin, Kap. 8 | AAR-Abschnitt „Plausibilitätscheck“ mit genau diesen Fragen, vom Umpire beantwortet. |
| Matrix Games sagen nicht die Zukunft voraus; ihr Wert liegt in Teilnahme und Einsichten. | Curry & Price, Introduction | Hinweis im AAR-Kopf; keine „Prognose“-Sprache in Prompts. |
| Wenige gute Gründe statt Wäscheliste; triviale Wiederholungen zählen nicht. | Curry & Price, Pros and Cons | Top-3-Regel, Gewicht 0. |
| Argumente, Pro/Contra und Umpire-Zusammenfassung mitschreiben; Nachbesprechung ist ausführlich. | Curry & Price, Introduction; Lasgah Pol | Journal ist die Mitschrift; AAR-Dokument. |
| Rollen auf gleicher Ebene; einheitlicher Zweck. | Curry & Price | Szenario-Validierung. |

### 8.2 AAR-Dokument

Am Spielende erzeugt der GameMaster `~/.harwness/matrix/<game-id>/aar.md` (plus `journal.jsonl`, `scenario.toml`-Kopie):

1. **Kopf**: Szenario, Zweck, Seed, Modelle, Rundenzahl, Hinweis „Einsichten, keine Prognose“.
2. **Endzustand** aller Tracks inkl. verdeckter, Verlaufsdiagramm als Tabelle je Runde.
3. **Zeitleiste**: jedes Argument mit Pros, Contras, Gewichten, Modifikator, Ziel, Wurf, Ergebnis, Umpire-Zusammenfassung.
4. **Offenlegung**: alle geheimen Argumente (mit Commitment-Prüfung), alle privaten Kanäle, alle `private_note`s — hier wird aus dem Nebel ein Lehrstück.
5. **Zielerreichung** je Fraktion (öffentlich/geheim, 0–3 mit Begründung).
6. **Schlüsselmomente & Alternativen**: Umpire nennt 2–3 Wendepunkte und schlägt Fork-Runden vor.
7. **Spieler-Debrief** (falls `debrief_players`): Jeder Spieler-Agent erhält die volle Offenlegung und beantwortet: Was wolltest du, was ist passiert, was hat dich überrascht, was würdest du anders machen?
8. **Plausibilitätscheck** (Sabins Validierungsfragen) und **Facilitator-Eingriffe** (alle Injects/Overrides).

---

## 9. Testbare Invarianten

### 9.1 Sichtbarkeit (Property-Tests, `proptest`)

Generator: zufällige Journale aus Events mit zufälligen Audiences, Spielern, Kanälen, geheimen Argumenten, Offenlegungen.

1. **Nicht-Interferenz**: Für jeden Spieler *p*: `project(J, p) == project(J ∪ E, p)` für jede Menge `E` von Events mit Audience `Pair(a,b)`, `p ∉ {a,b}`, sowie `Seat(q)`, `q ≠ p`, sowie `UmpireOnly`. Also ist die Projektion — und damit der gerenderte Prompt **byte-identisch** — unabhängig von fremden Gesprächen. Das deckt „weder Inhalt noch Existenz“ ab.
2. **Umpire-Modus `none`**: `project(J, Umpire)` invariant gegenüber allen `Pair`-Events.
3. **Umpire-Modus `cited`**: Umpire sieht Kanal *c* genau dann, wenn ein Argument der laufenden Adjudikation `c` zitiert und der Zitierende Mitglied von *c* ist.
4. **Monotonie**: Jedes Event in `project(J, p)` erfüllt `visible_to(aud, p)`; nach `SecretRevealed` wird das Event für alle sichtbar, vorher für niemanden außer Eigentümer und Umpire.
5. **Prompt-Scan**: Für generierte Szenarien mit eindeutigen Marker-Strings in jedem privaten/geheimen Inhalt kommt kein Marker in einem Prompt eines nicht berechtigten Sitzes vor (End-to-End über den echten Prompt-Builder mit Mock-Children).
6. **Weltvariablen**: `project` enthält eine Variable genau dann, wenn deren Sichtbarkeit den Sitz einschließt; geheime Argumente ändern keine öffentlich sichtbaren Variablen vor ihrer Offenlegung.

### 9.2 Determinismus & Replay

7. **Replay-Gleichheit**: Journal (inkl. journalisierter LLM-Antworten) + Szenario → Replay ohne Modellaufrufe liefert dieselben `state_hash`es an jedem Rundenende und dieselben Würfel.
8. **Reihenfolge-Unabhängigkeit**: Permutation der Fertigstellungsreihenfolge paralleler Child-Aufrufe ändert weder Würfel noch Zustand (abgeleitete Seeds; Einfügen in Journal in kanonischer Sitzreihenfolge).
9. **Override-Isolation**: Ein Override an Argument *x* ändert keine Würfel anderer Argumente.
10. **RNG-Verteilung**: Statistischer Test (10⁵ Seeds) — Häufigkeiten je Zielwert innerhalb ±1 % der Tabelle in §4.2; natürliche 2 scheitert immer.
11. **Fork**: Fork ab Runde N + gleiche Antworten ⇒ identisch zum Original bis N.

### 9.3 Versiegelte simultane Aktionen

12. **Siegel**: Kein Prompt der Phase *Argumente* enthält Inhalte eines Arguments derselben Runde (Test: Mock-Children zeichnen Prompts auf; Marker-Suche). `ArgumentsRevealed` wird erst journalisiert, wenn alle vier Einreichungen (oder `Forfeit`) vorliegen.
13. **Commitment**: Für jedes offengelegte geheime Argument gilt `sha256(canonical(content) ‖ salt) == commitment`; kanonische Serialisierung ist stabil (Golden-Test).
14. **Keine Nachbesserung**: Nach `ArgumentSealed` wird eine zweite Einreichung desselben Sitzes in derselben Runde abgewiesen.
15. **Effekte vor Wurf**: `DiceRolled` für Argument *x* steht im Journal immer nach dem `Adjudicated`, das `on_success`/`on_failure` für *x* enthält; angewandte Ops ⊆ gewählter Zweig (bzw. Override).

### 9.4 Schranken & Regeln

16. `target ∈ [3, 11]` für alle Gewichtskombinationen; Gewichte außerhalb {0,1,2} oder Modifikator außerhalb [−2,2] → Validierungsfehler.
17. Tracks bleiben immer in `[min, max]`; `|Δ| ≤ max_track_step` je Argument.
18. Big Projects: nie mehr als +1 Stufe pro Argument, nie > `stages`.
19. Secret-Limit je Sitz wird nie überschritten.
20. Leak-Guard: Ein Umpire-Text mit einem 5-Gramm aus einem unveröffentlichten Paar-Kanal wird nie als `Public` journalisiert (Mock-Umpire, der absichtlich leakt).

---

## 10. Nicht im ersten Schnitt

- Teams (mehrere Agenten pro Fraktion), > 4 Spieler, menschliche Spieler auf einem Sitz (Architektur lässt es zu: ein Sitz kann statt Child-Session die TUI als Eingabe haben).
- S.C.R.U.D.-Kampfauflösung (Curry & Price) — erst, wenn ein Szenario echte Gefechte braucht; wäre ein weiteres reines Rust-Modul auf demselben RNG.
- Simple-Narrative-System, Voting-Verfahren.
- Batch-Auswertung vieler Seeds.
- Karten/Graphen in der TUI (zunächst nur Tracks und Listen).

### Crate-Skizze

```
harw-matrix-game/
  src/scenario.rs     // TOML-Schema, Validierung
  src/audience.rs     // Seat, Audience, visible_to
  src/journal.rs      // Event, append-only JSONL, kanonische Serialisierung
  src/projection.rs   // project(), SeatView (einziger Prompt-Input)
  src/fsm.rs          // Phase, Übergänge, Facilitator-Befehle
  src/adjudicate.rs   // Gewichte → target, Konflikte, Fail-Chits
  src/rng.rs          // abgeleitete Seeds, ChaCha20
  src/effects.rs      // Op-Validierung, Delta-Anwendung, state_hash
  src/leak_guard.rs   // Shingle-Scanner
  src/prompts.rs      // Builder aus &SeatView
  src/contracts.rs    // Serde-Typen der JSON-Antworten
  src/aar.rs          // AAR-Markdown
  src/runner.rs       // Anbindung ChildController::run_child / run_children
```

Abhängigkeiten: `harw-core` (ChildController), `harw-types`, `serde`, `toml`, `sha2`, `rand_chacha`; `harw-tui` hängt von `harw-matrix-game` nur über einen Beobachter-Stream (`project(Observer)`) plus Befehlskanal ab.

---

## 11. Erweiterungen: Verhalten, Red Cell, Verdacht, Präzedenz, Varianz

Alle fünf Bausteine sind optional; ein Szenario ohne sie verhält sich byte-gleich wie zuvor (neue Zustandsfelder werden leer nicht serialisiert, `state_hash` älterer Journale bleibt gültig). Jede Wirkung läuft über Journal-Einträge; Prompts entstehen weiterhin nur aus `SeatView`.

### 11.1 Verhaltensprofil je Fraktion/Team

`[factions.behavior]` bzw. `[teams.behavior]` direkt unter dem jeweiligen Eintrag: `rules` (≤ 6 Handlungsregeln), `risk` (0 = meidet Risiko … 1 = sucht es; Default 0,5), `loss_framing` (Lage als drohender Verlust gegenüber dem Bezugspunkt erlebt), `anchor` (Bezugspunkt), `red_lines` (2–5, Pflicht, sobald ein Profil existiert). Das Profil ist privat: ausführlich im System-Prompt dieses Sitzes, als `BehaviorBriefing` mit Audience `Seat(p)` im Journal und daraus als Kurz-Erinnerung (Risiko, Rahmung, Anker, rote Linien) am Anfang **jedes** Zug-Prompts dieses Sitzes — auch im reinen Delta. Umpire, Red Cell und andere Sitze sehen es nie; das AAR legt es offen.

### 11.2 Red Cell

`[red_cell] enabled = true, sharpness = 0..1` (nur mit `argument_system = "pros_cons"`). Neuer Sitz `Seat::RedCell` (Agentenrolle `matrix-redcell`): sieht ausschließlich Öffentliches, hat keine Ziele und keine Siegbedingung. In der Gegenargument-Phase folgt nach allen Spielern ein Aufruf mit Vertrag `red_cell_objection`: `{target, assumption, cons}` gegen die tragende Annahme des führenden öffentlichen Arguments — oder ausdrücklich `{"no_objection": true}`. Die Schärfe bestimmt Ton und Höchstzahl der Contras (1/2/3). `submit_red_cell` journalisiert öffentlich (`RedCellObjection`) und hängt die Contras unter dem reservierten Schlüssel `red_cell` an das Ziel; der Umpire gewichtet sie in `con_weights["red_cell"]` wie jedes Contra (`red_cell` ist als Sitz-ID gesperrt und kein Eintrag in `standing`).

### 11.3 Verdachtsleiter für geheime Argumente

Stufen je Geheimnis: unbemerkt → Gerücht → Verdacht → Belege → aufgeflogen. Effekt-Op `{"op": "raise_suspicion", "secret_id", "by": 1|2}` — nur in Urteilen über öffentliche Argumente oder in Injects, nicht in `each_round`, höchstens zwei Stufen je Effekt-Zweig und Geheimnis. Öffentlich erscheint ausschließlich eine feste Rust-Vorlage (`SuspicionRaised`), die außer der ohnehin angekündigten Geheimnis-ID nichts enthält. Auf der obersten Stufe folgt die reguläre Offenlegung mit Salt (`RevealedBy::Suspicion`). Die Lage steht im Lagebild aller Sitze und im Adjudikations-Auftrag des Umpires.

### 11.4 Präzedenzregister

Der Umpire markiert ein Urteil über ein **öffentliches** Argument mit `precedent: {principle, tags}` (1–5 Schlagworte; bei geheimen Argumenten ein Contract-Fehler). Rust journalisiert es öffentlich als `PrecedentSet` (`p1`, `p2` …, mit Netto und Wahrscheinlichkeit). Spätere Adjudikations-Prompts nennen bis zu fünf einschlägige Maßstäbe früherer Runden — deterministisch gewählt über Schlagworttreffer und Überlappung langer Stichwörter mit den Argumenten der Runde, ausschließlich aus der Umpire-Projektion. `principle` ist öffentlicher Text und läuft wie `public_rationale` durch den Leak-Guard. Das AAR führt ein Präzedenzregister samt späteren Fällen, auf die ein Maßstab gepasst hätte.

### 11.5 Inject-Bibliothek, Mehrfachläufe, Design-Lehren

`[[inject_packages]]` mit `id`, `label`, `max_injects` (1–3) und Kandidaten `[[inject_packages.injects]]` (`id`, `text`, `audience`, `effects`, optional `earliest`/`latest`, `attributed`). Ein Lauf wählt ein Paket (vorgegeben oder per Seed) und zieht daraus höchstens drei Injects, nie zwei in derselben Runde und nie in einer Runde mit festem Szenario-Inject. Die Auswahl hängt nur von Master-Seed und Szenario ab, steht als `InjectPackageSelected` (nur Beobachter) im Journal und wird beim Replay nachgerechnet; fällig werdende Injects liefert `package_injects_for_round`.

Das AAR erhält den Abschnitt **Design-Lehren**: Zeitpunkt der geheimen Argumente (und ob sie erst zum Spielende aufflogen), Klumpung der Zielwerte/Leiterstufen, Plausibilität der Würfel (erste Würfe: mittlere Augensumme und Erfolge gegen Erwartung als z-Wert) sowie daraus abgeleitete Hinweise an das Szenario-Design. Für Mehrfachläufe über Seeds verdichtet `summarize_run` jeden Lauf zu einer `AarSummary`; die reine Funktion `compare_runs(&[AarSummary])` stellt Kennzahlen und Endwerte nebeneinander und trennt robuste von empfindlichen Größen. In der Oberfläche vergleicht `/matrix compare <lauf> <lauf> …` die Läufe der Sitzung und legt den Bericht als `compare-<läufe>.md` neben die Laufverzeichnisse; `/matrix start <szenario> --package <id>` wählt ein Inject-Paket ausdrücklich (ohne Angabe wählt der Seed).
