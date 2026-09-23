# Business-Wargaming, analytisches Handwerk und Visualisierung

Status: Entwurf · Stand 2026-09-23 · Ergänzt `docs/design/matrix-game.md` (Matrix-Game-Kern: GameMaster, Spieler-Agenten, Szenario-Schema `harwness.matrix-scenario/v1`). Wo dieses Dokument Typ- oder Feldnamen des Kerns voraussetzt, gilt bei Abweichung `matrix-game.md`; die hier eingeführten Erweiterungen sind additiv formuliert.

Quellen (Studienmaterial, sinngemäß und in eigenen Worten wiedergegeben):

- Oriesek & Schwarz, *Business Wargaming* (Teams, Züge, Gamebook, Debriefing, Fallstudien Mobilfunk/Automobil, Frühwarnsysteme).
- Hall & Citrenbaum, *Intelligence Analysis: How to Think in Complex Environments* (Dekomposition von Hypothesen in Beobachtbares, kritisches Denken, Mirror Imaging, Annahmenprüfung, Red Teaming).
- Herman, *Intelligence Power in Peace and War* (Single-Source vs. All-Source, Trichter der Veredelung, Intelligence-Zyklus und seine Grenzen, Need-to-know).
- Zelazny, *Say It With Charts* (Botschaft → Vergleichsart → Diagrammform).
- Ergänzend, nicht aus dem Material: Analysis of Competing Hypotheses (Heuer), Admiralty-/NATO-Quellenbewertung (A–F/1–6). Beides ist gängige Praxis und wird hier als Standardtechnik behandelt.

---

## 0. Kurzfassung

1. **Business-Modus** (`mode = "business"`) macht aus dem Matrix Game ein Oriesek-artiges Strategie-Wargame: Unternehmens-, Wettbewerber- und Marktteam als Spieler-Agenten, der Rust-`GameMaster` als Control/White Cell, drei Züge mit simulierten Zeitsprüngen, Entscheidungsvorlagen statt Freitext, ein pragmatisches Marktmodell mit menschlich/agentisch überstimmbarer Bewertung, Schocks als Injects, strukturiertes Debriefing (Beobachtung → Lehre → Implikation → Empfehlung).
2. **Analytisches Handwerk** wird als vier plus zwei Skills (`analysis-ach`, `key-assumptions`, `red-team`, `indicators-warnings`, `source-grading`, `hypothesis-decomposition`) und als additive Felder im `ResearchFinding` kodiert (Hypothesen, Annahmen, Indikatoren, Quellenbewertung, Wahrscheinlichkeitssprache getrennt von Konfidenz).
3. **Intelligence-Zyklus**: Orchestrator = Tasking/All-Source-Analyse, Worker = Single-Source-Sammlung + Erstverarbeitung. Need-to-know ist bereits Kernprinzip (`ContextCeiling::intersect`, `VisibilityScope`) und wird auf die Sichtbarkeit im Matrix Game übertragen.
4. **Visualisierung**: jede Grafik hat einen Botschaftstitel; Vergleichsart bestimmt das ratatui-Widget.
5. **Priorisierte Umsetzung** in §5.

---

## 1. Business-Wargame-Modus für das Matrix Game

### 1.1 Was ein Business-Wargame von einem Planspiel unterscheidet

Oriesek/Schwarz grenzen das Business-Wargame deutlich von Business-School-Simulationen ab: Dort liegen die „richtigen" Stellschrauben im Modell und sind nach wenigen Runden durchschaut. Ein Wargame dagegen wird für **eine konkrete Organisation und konkrete Schlüsselfragen** entworfen, die Erkenntnis entsteht aus der Interaktion der Teams, und das Rechenmodell ist nur Hilfsmittel des Control-Teams, das es jederzeit korrigieren darf. Für uns folgt daraus:

- Das Marktmodell im `GameMaster` ist **deterministisch, klein und überstimmbar** – es liefert einen Vorschlag, keine Wahrheit. Jede Überstimmung wird mit Begründung protokolliert (Nachvollziehbarkeit war ein Erfolgsfaktor im Buch: wenn keine harten Daten da sind, muss man wenigstens rückwärts erklären können, warum etwas so ausging).
- Jedes Szenario beginnt mit **Schlüsselfragen** (`key_questions`), die das Debriefing am Ende explizit beantworten muss. Ohne sie ist das Spiel nicht startfähig (Validierungsfehler).

### 1.2 Rollenmodell → unsere Topologie

Das Buch nennt vier Grundelemente: Unternehmensteam, Wettbewerberteams, Marktteam, Control-Team. Control spielt zusätzlich alle nicht explizit besetzten Stakeholder (Regulator, kleinere Wettbewerber, Zielunternehmen einer Übernahme, Interessengruppen) und kann Schocks einspielen.

| Buch-Rolle | Harwness | Umsetzung |
|---|---|---|
| Unternehmensteam („Blue") | Spieler-Agent `company` | spielt den aktuellen Strategieplan (oder eine Alternative, `strategy_variant`) |
| Wettbewerber 1..n („Red") | Spieler-Agenten `competitor_a`, `competitor_b` | erhalten Gamebook-Profil; Auftrag „Be the enemy" |
| Marktteam | Spieler-Agent `market` **oder** White-Cell-Subagent des GM | vergibt relative Attraktivität/Marktanteile |
| Control / White Cell | Rust-`GameMaster` (+ optionaler LLM-Adjudikator) | Zeitplan, Regeln, KPI-Rechnung, Schocks, Rest-Stakeholder, Offenlegung |
| Regulator, Kleinwettbewerber, Übernahmeziele | vom `GameMaster` gespielt (`[[business.stakeholders]]`) | Entscheidungen per Regel + Adjudikator-Prompt |
| Coach / Devil's Advocate je Team | Skill `red-team` am Spieler-Agenten | stellt Annahmen in Frage, erzwingt Vorlagen-Disziplin |

Bei **vier Spieler-Slots** gibt es zwei sinnvolle Belegungen (Szenario-Feld `business.market_role`):

- `market_role = "player"` (Standard): company, competitor_a, competitor_b, market. Der Markt ist dann ein bewertender Spieler – er reicht keine Angebote ein, sondern Bewertungen (eigene Vorlage `market_assessment`).
- `market_role = "white_cell"`: der Markt wird als interner Subagent des GM gefahren; der vierte Slot wird ein dritter Wettbewerber. Die Automobil-Fallstudie im Buch zeigt ein bewährtes Trio: der tatsächliche Hauptwettbewerber, ein Neueinsteiger aus einem Nachbarmarkt und ein fiktiver aggressiver Billiganbieter.

Das Marktteam ist Bewerter, nicht Konkurrent; es darf daher keine privaten Kanäle zu Anbieterteams nutzen, außer für explizit modellierte Kundengespräche (Kanaltyp `sales_call`, vom GM mitgelesen).

### 1.3 Zugstruktur

Laut Buch sind drei Züge üblich; der erste startet in der Gegenwart auf Basis realer Daten, spätere Züge springen in die Zukunft (z. B. 1 Jahr, 2 Jahre, 4+ Jahre, der letzte als „Foresight"). Ein Zug ist ein Entscheidungszyklus. Die Mobilfunk-Fallstudie gliedert: Markteintritt – Positionierung gegen Wettbewerber – langfristige Verfestigung.

Abbildung auf GM-Phasen je Zug (`MovePhase` im GM-Zustandsautomaten):

```
Brief ─▶ Deliberation ─▶ Submission ─▶ Plenary ─▶ MarketAssessment ─▶ Adjudication ─▶ Hotwash ─▶ (nächster Zug)
  │          │ private Paarkanäle (GM cc)   │ öffentliche Pitches    │ Marktbewertung   │ KPI-Rechnung, Schocks
  │          │ Entscheidungsvorlage          │ (alle sehen alles)     │                  │ Offenlegung
  └ Zug-Briefing: öffentlicher Lagebericht + privater Team-Bericht (KPIs, P&L-Auszug)
```

- **Brief**: GM verteilt `public_situation` (alle) und `team_report` (nur Team) – Startpunkt des Zuges ist das Ergebnis des Vorzugs. Wer Anteile „gekauft" hat, sieht jetzt die leere Kasse und eingeschränkte Optionen (Budgetgrenze aus `cash`).
- **Deliberation**: Team-interne Überlegung (ein Agent pro Team; optional Kind-Agenten als Teamrollen: Leiter, Briefer, Kommunikator – die Mobilfunk-Fallstudie nennt genau diese drei plus Coach). Kontakt zu anderen Teams **nur** über private Paarkanäle; der GM ist in jedem Kanal stiller Mitleser (im Buch: jede E-Mail zwischen Teams geht automatisch in Kopie an Control).
- **Submission**: strukturierte Entscheidungsvorlage (§1.5). Fristen sind hart; ein verspätetes Team erhält die Vorzugsentscheidung als „Status quo" (das Buch betont, dass ein nachhinkendes Team das ganze Spiel aus dem Takt bringt).
- **Plenary**: Jedes Anbieterteam liefert einen öffentlichen Pitch (Nutzenversprechen, max. N Tokens). Ab hier haben alle denselben Informationsstand über Angebote.
- **MarketAssessment**: Marktteam bewertet je Segment Attraktivität (0–10) und begründet; Rechenmodell erzeugt Anteilsvorschlag.
- **Adjudication**: GM konsolidiert harte Daten (Preis, Invest) und weiche Daten (Marktbewertung) zu KPIs, prüft Deals gegen Regeln (Kartellrecht, Finanzkraft), entscheidet Stakeholder-Reaktionen, spielt fällige Injects ein.
- **Hotwash**: kurze Zwischenlehren nach jedem Zug (Buch: nach jedem Zug eine erste Zusammenfassung). Jeder Spieler liefert drei Sätze „was hat funktioniert / was nicht / was überrascht"; GM hängt sie ans AAR-Protokoll.

**Variante „attack-counter"** (`business.variant = "attack_counter"`, nach der Automobil-Fallstudie): Runde 1 greifen Wettbewerber an (mit fiktiv unbegrenztem Kapital, aber legal und plausibel), Runde 2 entwickelt das Unternehmen Gegenstrategien, Runde 3 reagieren die Angreifer auf eine „abgefangene" Gegenstrategie. Ein „Botschafter" trägt die Absicht einer Gruppe in die nächste – bei uns eine GM-generierte Zusammenfassung, die als `ambassador_brief` in den Kontext des Gegenübers geht. Diese Variante braucht kein Marktteam und passt gut zu Blind-Spot-Analysen.

### 1.4 Injects (Markt- und Szenario-Schocks)

Das Buch beschreibt Schocks als Eingriffe von Control, die Teams zwingen, ein Thema zu adressieren (Produktrückruf, Sicherheitsbedenken, Regulierungsänderung). Wir unterscheiden:

| Art | Auslöser | Sichtbarkeit | Beispiel |
|---|---|---|---|
| `scheduled` | fester Zug/Phase | public oder Team-Liste | Marktöffnung Privatkunden ab Zug 2 |
| `conditional` | Prädikat über Weltzustand | public | Anteil eines Teams > 40 % → Kartellprüfung |
| `random` | seed-deterministischer Wurf mit `p` | privat | Lieferantenausfall bei `competitor_b` |
| `control_discretion` | Adjudikator-Vorschlag, vom GM bestätigt | variabel | Presseleak eines Allianzgesprächs |

Alle Injects sind im Szenario deklariert; der Adjudikator darf nur **aus dem Katalog** wählen (kein Erfinden neuer Weltfakten durch das LLM). Zufall ist seed-basiert, damit ein Spiel reproduzierbar erneut gespielt werden kann – das Buch berichtet, dass wiederholte Spiele mit anderen Teilnehmern zwar anders verlaufen, aber ähnliche Lehren ergeben; genau diese Robustheit wollen wir mit `harw matrix replay --seed` messen können.

### 1.5 Strategische Entscheidungsvorlage

Im Buch sind Vorlagen das Medium, über das Teams dem Markt und Control harte Daten liefern. Bei uns ist die Vorlage ein Tool-Aufruf `submit_move` mit JSON-Schema (vom GM aus dem Szenario generiert):

```json
{
  "team": "competitor_a",
  "move": 2,
  "strategic_intent": "Preisführerschaft im KMU-Segment, Premium halten",
  "actions": [
    {"kind": "price",    "segment": "sme",     "offer": "cloud_basic", "value": 19.0},
    {"kind": "invest",   "area": "sales",      "amount_meur": 12},
    {"kind": "product",  "offer": "cloud_ai",  "change": "launch", "segments": ["enterprise"]},
    {"kind": "alliance", "with": "company",    "proposal_ref": "ch-a-c/msg-14"},
    {"kind": "lobby",    "target": "regulator", "topic": "data_residency"}
  ],
  "assumptions": [
    {"id": "A1", "text": "company senkt nicht vor Zug 3 die Preise", "confidence": "medium"}
  ],
  "expected_reactions": {"company": "Bündelangebot", "competitor_b": "Nischenrückzug"},
  "expected_kpis": {"share_sme": 0.30, "ebit_meur": 4.0},
  "pitch": "…öffentlicher Text für das Plenum…"
}
```

Wichtige Regeln:

- `actions[].kind` ist ein geschlossener Satz aus `business.action_kinds`; unbekannte Aktionen gehen als `free_argument` in die klassische Matrix-Game-Adjudikation (Argument + Gründe, siehe unten).
- `assumptions` und `expected_reactions` sind Pflicht. Sie sind der Rohstoff fürs Debriefing: der GM vergleicht nach dem Zug Erwartung und Ergebnis („Prognosefehler je Team").
- Budgetprüfung: Summe `invest` ≤ verfügbare Mittel; sonst Ablehnung mit Fehlermeldung an den Agenten (ein Korrekturversuch, dann Status quo).

### 1.6 Adjudikation im Business-Kontext

Drei Schichten, in dieser Reihenfolge:

1. **Regelprüfung (Rust, deterministisch)**: Budget, Kapazität, Regulierung (`business.rules`), Kartellschwellen. Deals zwischen Teams brauchen beiderseitige `alliance`/`acquisition`-Aktionen mit gleicher `proposal_ref`.
2. **Marktmodell (Rust, deterministisch)**: Attraktivitätsmodell je Segment, z. B. Anteil_i ∝ exp(β · Score_i), Score = gewichtete Summe aus Marktbewertung (weich), Preisposition, Vertriebsinvest (mit abnehmendem Grenznutzen), Produktfit. Ausgabe: Anteile, Umsatz, Deckungsbeitrag, EBIT, Cash. Bewusst wenige KPIs – das Buch empfiehlt pragmatische Modelle, die sich auf die Schlüsselfragen beschränken.
3. **Control-Urteil (LLM-Adjudikator, optional, gebunden)**: Darf Modellergebnisse innerhalb `business.override_bounds` (z. B. ±5 Prozentpunkte Anteil) korrigieren und muss dafür eine Begründung schreiben (`AdjudicationOverride { field, from, to, rationale }`). Das Buch schildert genau diesen Fall: Ein Team wählte eine extreme Billigstrategie, das Kostenmodell hätte sie zu Unrecht bestraft, also passte Control Kosten manuell an. Nicht katalogisierte Aktionen (`free_argument`) werden nach Matrix-Game-Logik bewertet: Argument mit Gründen, Gegenargumente der betroffenen Teams, dann Wahrscheinlichkeit (gestaffelt `almost_certain`/`likely`/`even`/`unlikely`/`remote`) und seed-deterministischer Wurf.

Stakeholder-Entscheidungen (Regulator genehmigt Übernahme nur mit Auflagen, Zielvorstand lehnt ab) trifft der GM per Regel oder Adjudikator mit Stakeholder-Profil aus dem Szenario.

### 1.7 Informationsgleichheit und Offenlegung

Im Buch verbreitet Control bestimmte Informationen aktiv weiter (etwa eine geschlossene Allianz), weil sie in der Realität über die Medien bekannt würden – so bleibt Informationsgleichheit gewahrt. Umsetzung als deklarative Offenlegungsregeln:

```toml
[[business.disclosure]]
on = "alliance_signed"      # Ereignis im GM
audience = "all"            # oder Team-Liste
delay_phases = 0
template = "{a} und {b} geben eine Partnerschaft bekannt: {summary}"
```

Private Kanalinhalte werden **nie** wörtlich offengelegt, nur als vom GM formulierte Meldung (siehe Compartmentation §3.3).

### 1.8 Debriefing und Insight Capture

Das Buch hält das detaillierte Debriefing für den wichtigsten Teil; es stützt sich auf Coach-Beobachtungen, Teilnehmer-Input, Modelldaten und die Auswertung des gesamten E-Mail-Verkehrs (wer mit wem worüber, Muster wie „alle suchen im selben Zug ähnliche Allianzen"). Die Struktur ist deduktiv: Beobachtungen → Lehren → Konsequenzen → konkrete Empfehlungen zurück in den Strategieplan.

Wir erzeugen ein AAR-Artefakt `harwness.matrix-aar/v1` (JSON + gerendertes Markdown):

```rust
pub struct BusinessAar {
    pub scenario_id: String,
    pub seed: u64,
    pub key_questions: Vec<KeyQuestionAnswer>,   // je Schlüsselfrage: Antwort + Belege (Zug/Kanal/Event-IDs)
    pub observations: Vec<Observation>,          // faktisch, mit Event-Referenz
    pub lessons: Vec<Lesson>,                    // verweist auf observation_ids
    pub implications: Vec<Implication>,          // für das Unternehmen
    pub recommendations: Vec<Recommendation>,    // umsetzbar, mit Owner-Vorschlag
    pub forecast_errors: Vec<ForecastError>,     // expected_kpis/expected_reactions vs. Ergebnis
    pub channel_patterns: Vec<ChannelPattern>,   // Kontaktgraph je Zug, Gleichklang von Deals
    pub bias_checks: Vec<BiasCheck>,             // siehe unten
    pub indicators: Vec<IndicatorRef>,           // Übergabe an indicators-warnings (§2)
}
```

- **Kanalanalyse** ist rein deterministisch (Rust): Kontaktmatrix Team×Team je Zug, Nachrichtenzahl, erste Erwähnung von Deal-Stichworten. Das LLM interpretiert nur.
- **Bias-Checkliste** aus der Automobil-Fallstudie, in eigenen Worten: Wahrscheinlichkeitsdenken (man sucht das Erwartbare statt des Überraschenden), Gruppendenken, blinde Flecken, Erfahrungsfixierung, Hang zum Spektakulären (kleine Züge werden unterschätzt), Produktverliebtheit (Kunden kaufen Nutzen, nicht Produkte). Jeder Punkt wird im AAR mit „beobachtet / nicht beobachtet + Beleg" beantwortet.
- **Gegenstrategien** klassifiziert das AAR nach den sieben Umgangsformen mit Überraschungen aus der Fallstudie: ignorieren (als unerheblich einstufen), verhindern (Voraussetzungen beseitigen), vorsorgen (Notfallplan), vorbereiten (Elemente in die laufende Strategie einbauen), umwandeln (Überraschung zur Chance machen), abmildern (Schaden begrenzen, z. B. Versicherung), vorwegnehmen (die befürchtete Aktion selbst ausführen). Rust-Enum `CounterApproach { Neglect, Prevent, Provide, Prepare, Convert, Reduce, Anticipate }`.
- **Brücke zur Frühwarnung**: Das Buch schlägt vor, die im Wargame identifizierten Bedrohungs- und Chancenfelder als Suchfelder eines strategischen Frühwarnsystems zu nutzen (schwache Signale, Umfeldscanning). Jede Empfehlung kann Indikatoren tragen, die der Skill `indicators-warnings` in beobachtbare Signale zerlegt und die als Kanban-Karten/Knowledge-Einträge (`harw-knowledge`) weiterleben.

Persistenz: AAR und vollständiges Ereignisprotokoll (inkl. Kanalinhalte) unter `knowledge_dir/matrix/<scenario_id>/<run_id>/`, Sichtbarkeit `VisibilityScope` des Spielleiters; Spieler-Agenten sehen nach Spielende nur das öffentliche AAR („Endex-Freigabe" konfigurierbar).

### 1.9 Vollständiges Beispiel-Szenario

```toml
schema = "harwness.matrix-scenario/v1"
id = "cloud-sme-2027"
title = "KMU-Cloud: Markteintritt eines Hyperscalers"
mode = "business"
seed = 20270101
language = "de"

[game]
moves = 3
# simulierte Zeit je Zug (Zug 3 = Foresight)
move_labels = ["2027 – Markteintritt", "2028 – Positionierung", "2031 – Verfestigung"]
phase_deadline_turns = 6          # max. Agenten-Turns je Phase und Team
max_channel_messages_per_move = 12

[[game.key_questions]]
id = "KQ1"
text = "Hält unsere Premium-Positionierung, wenn ein Hyperscaler KMU-Preise um 30 % unterbietet?"
[[game.key_questions]]
id = "KQ2"
text = "Lohnt eine Allianz mit einem regionalen Systemhaus-Verbund gegenüber eigenem Vertriebsausbau?"
[[game.key_questions]]
id = "KQ3"
text = "Welche Regulierung (Datenresidenz) verändert die Spielregeln, und wer profitiert?"

[business]
variant = "strategy_test"         # | "attack_counter" | "crisis_response"
market_role = "player"            # | "white_cell"
currency = "MEUR"
override_bounds = { share_pp = 5.0, cost_pct = 15.0 }
action_kinds = ["price", "invest", "product", "alliance", "acquisition", "lobby", "campaign", "free_argument"]

[business.market_model]
kind = "logit_share"
beta = 0.9
weights = { market_score = 0.45, price_position = 0.30, sales_invest = 0.15, product_fit = 0.10 }
sales_invest_saturation_meur = 20
kpis = ["share", "revenue", "contribution_margin", "ebit", "cash"]

[[business.segments]]
id = "sme"
name = "KMU (10–249 MA)"
size_meur = [800, 950, 1300]      # je Zug
price_sensitivity = 0.8
[[business.segments]]
id = "enterprise"
name = "Großkunden"
size_meur = [1200, 1260, 1400]
price_sensitivity = 0.35

[[business.rules]]
id = "antitrust"
when = "share(segment) > 0.40 && action.kind == 'acquisition'"
effect = "require_stakeholder:regulator"
[[business.rules]]
id = "cash_floor"
when = "cash < 0"
effect = "restrict_actions:[invest,acquisition]"

[[teams]]
id = "company"
role = "company"
display = "NordCloud AG (wir)"
agent = "matrix-player"
strategy_brief = "gamebook/nordcloud.md"
start = { cash = 60, share = { sme = 0.34, enterprise = 0.22 }, capacity = 1.0 }

[[teams]]
id = "competitor_a"
role = "competitor"
display = "Hyperscaler X (Neueinsteiger KMU)"
agent = "matrix-player"
strategy_brief = "gamebook/hyperscaler-x.md"
directive = "Be the enemy: gewinne KMU-Anteile legal, plausibel, aggressiv."
start = { cash = 500, share = { sme = 0.05, enterprise = 0.30 }, capacity = 3.0 }

[[teams]]
id = "competitor_b"
role = "competitor"
display = "ValueHost GmbH (Billiganbieter, fiktiv)"
agent = "matrix-player"
strategy_brief = "gamebook/valuehost.md"
start = { cash = 25, share = { sme = 0.28, enterprise = 0.03 }, capacity = 0.8 }

[[teams]]
id = "market"
role = "market"
display = "Marktteam (KMU- und Großkunden-Panel)"
agent = "matrix-market"
strategy_brief = "gamebook/market-panel.md"
assessment_scale = [0, 10]

[[business.stakeholders]]
id = "regulator"
display = "Aufsichtsbehörde Datenschutz/Wettbewerb"
played_by = "gamemaster"
profile = "Prüft Übernahmen ab 40 % Segmentanteil; sensibel für Datenresidenz."
[[business.stakeholders]]
id = "sysint_alliance"
display = "Verbund regionaler Systemhäuser"
played_by = "gamemaster"
profile = "Sucht exklusiven Plattformpartner; verlangt 20 % Marge und Leadschutz."

[channels]
pairwise = "all_players"          # private Paarkanäle zwischen allen Teams
observer = "gamemaster"           # GM liest jeden Kanal mit (cc Control)
market_contact = "sales_call_only"

[[business.disclosure]]
on = "alliance_signed"
audience = "all"
delay_phases = 0
template = "{a} und {b} verkünden eine Partnerschaft: {summary}"
[[business.disclosure]]
on = "price_change"
audience = "all"
delay_phases = 1                  # Listenpreise sind nach einer Phase öffentlich

[[injects]]
id = "INJ-1"
kind = "scheduled"
at = { move = 2, phase = "brief" }
audience = "all"
text = "Gesetzentwurf: Personenbezogene KMU-Daten müssen ab 2029 im Inland verarbeitet werden."
[[injects]]
id = "INJ-2"
kind = "conditional"
when = "share('competitor_a','sme') > 0.25"
audience = "all"
text = "Fachpresse: Kartellbehörde prüft Kampfpreise im KMU-Cloudmarkt."
[[injects]]
id = "INJ-3"
kind = "random"
p = 0.25
at = { move = 3, phase = "brief" }
audience = ["competitor_b"]
text = "Ihr Rechenzentrumsbetreiber kündigt; 20 % Kapazität fallen für einen Zug aus."
[[injects]]
id = "INJ-4"
kind = "control_discretion"
audience = "all"
text = "Ein Allianzgespräch wird öffentlich geleakt."

[debrief]
hotwash_each_move = true
bias_checklist = true
counter_approach_taxonomy = true
export_indicators = true
endex_release = "public_aar_only"
```

Gamebooks (`gamebook/*.md`) sind Teil des Szenario-Pakets: je Team ein Einseiter-Profil (Strategie, Finanzen, Stärken/Schwächen), plus ein gemeinsamer Marktüberblick. Das Buch betont, dass alle Teilnehmer dieselbe Grundlage erhalten; teamprivate Zusatzinformationen gehören in `strategy_brief` und sind nur dem Team sichtbar.

Validierung (GM beim Laden): `key_questions` ≥ 1; genau ein `company`; `market_role = "player"` ⇒ genau ein Team `role = "market"`; Summen Startanteile je Segment ≤ 1; alle `injects.when`/`rules.when` parsebar; `size_meur`-Länge = `moves`.

---

## 2. Analytisches Handwerk für Analyst-/Researcher-/Explorer-Agenten

### 2.1 Auswahl der Techniken

| Technik | Kerngedanke (eigene Worte) | Form | Warum |
|---|---|---|---|
| Hypothesen-Dekomposition | Hall/Citrenbaum: eine Hypothese in erwartbare Aktivitäten, Transaktionen, Verhalten zerlegen („wenn das stimmt, müsste ich X sehen") und damit die Sammlung steuern | Skill `hypothesis-decomposition` | macht Explorer-Aufträge prüfbar |
| Analysis of Competing Hypotheses | mehrere Hypothesen gegen alle Belege matrixartig prüfen, die am wenigsten widerlegte gewinnt; Diagnostizität statt Bestätigung | Skill `analysis-ach` + Finding-Feld `hypotheses` | wirkt dem Bestätigungsfehler entgegen |
| Key Assumptions Check | Annahmen explizit machen, wie Hypothesen testen, in Beobachtbares zerlegen (Hall: schlechte Annahmen sind oft der Single Point of Failure) | Skill `key-assumptions` + Feld `key_assumptions` | Annahmen verschwinden sonst im Fließtext |
| Red Team / Devil's Advocate | Gegenposition aus Sicht des Gegenübers, gezielt gegen Mirror Imaging | Skill `red-team`; im Matrix Game als Coach | Hall nennt Red Teams wiederholt als Gegenmittel |
| Indicators & Warnings | Beobachtbare Signale je Szenario/Annahme mit Schwelle und Prüffrequenz; Frühwarnung vor Überraschung | Skill `indicators-warnings` + Feld `indicators` | verbindet Wargame-AAR mit Monitoring |
| Quellenbewertung | Zuverlässigkeit der Quelle (A–F) getrennt von Glaubwürdigkeit der Information (1–6); Unabhängigkeit prüfen | Skill `source-grading` + Felder an `SourceReference` | Herman: Collectors prüfen ihre Quellen selbst auf Unzuverlässigkeit/Täuschung |
| Wahrscheinlichkeitssprache | Herman: Aufgabe der Analyse ist, die Spannweite der Unsicherheit möglichst genau zu vermitteln, meist über kodierte Begriffe | Feld `likelihood` (neben `confidence`) | trennt „wie wahrscheinlich" von „wie belastbar" |

Nicht übernommen (vorerst): kulturelle/semiotische Analyse aus Hall – für Code-/Dependency-Recherche ohne Nutzen; Anomalie- und Trendanalyse gehen implizit in `indicators-warnings` auf (Basislinie + Abweichung).

### 2.2 Skill-Manifeste

Format folgt `harw-config/src/skill_toml.rs` (`name`, `enabled`, `description`, `instructions_file` – Default `instructions.md`, `tools`, `mcps`; `deny_unknown_fields`). Alle Skills sind **read-only**; sie verändern nichts, sondern strukturieren Denken und Rückgabe.

#### `harw-home/assets/skills/analysis-ach/skill.toml`

```toml
name = "analysis-ach"
description = "Analysis of Competing Hypotheses: mehrere Erklärungen gegen alle Belege prüfen, Diagnostizität statt Bestätigung."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

`instructions.md` (Gliederung):
1. **Wann**: Frage hat ≥ 2 plausible Erklärungen (Ursache eines Fehlers, Absicht eines Wettbewerbers, Grund einer Regression).
2. **Hypothesen bilden**: mindestens 3, sich gegenseitig ausschließend, inkl. einer „unbequemen" und einer Null-Hypothese; je `H1..Hn` mit einem Satz.
3. **Belegliste**: jedes Beweisstück einmal, mit Quellbewertung (Verweis `source-grading`); auch Fehlen erwarteter Belege zählt.
4. **Matrix**: je Beleg × Hypothese `consistent | inconsistent | neutral`; Belege, die überall konsistent sind, als nicht-diagnostisch markieren.
5. **Bewertung**: Hypothesen nach Zahl gewichteter Inkonsistenzen ordnen; nie nach Zahl der Bestätigungen.
6. **Sensitivität**: Welche 1–2 Belege entscheiden? Was, wenn sie falsch/manipuliert sind?
7. **Rückgabe**: `hypotheses[]` mit `status` und `inconsistency_score`, Schluss nennt führende Hypothese + nächste Sammlungsaufgabe, die H1 vs. H2 am besten trennt.
8. **Anti-Patterns**: nur eine Hypothese; Belege nach Passung auswählen; Konfidenz `high` bei zwei gleichauf liegenden Hypothesen.

#### `harw-home/assets/skills/key-assumptions/skill.toml`

```toml
name = "key-assumptions"
description = "Tragende Annahmen offenlegen, begründen, als Hypothese testen und in beobachtbare Prüfsignale zerlegen."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

Gliederung: (1) Schlussfolgerung notieren; (2) jede unausgesprochene Voraussetzung als Satz formulieren (Trigger-Formulierungen: „natürlich", „wird sicher", „wie immer"); (3) je Annahme: *Warum glauben wir das? Unter welchen Umständen wäre sie falsch? War sie früher schon falsch?*; (4) Einstufung `supported | caveated | unsupported`; (5) Mirror-Imaging-Check nach Hall – Warnsätze wie „das würden die nie tun, weil es keinen Sinn ergibt"; (6) jede `unsupported`-Annahme bekommt ≥ 1 Indikator; (7) Rückgabe `key_assumptions[]`; Regel: eine `unsupported` Annahme auf dem kritischen Pfad deckelt `confidence` auf `medium`.

#### `harw-home/assets/skills/red-team/skill.toml`

```toml
name = "red-team"
description = "Gegenposition aus Sicht des Gegenübers einnehmen, Mirror Imaging und Gruppendenken aufdecken, stärkste Alternative formulieren."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

Gliederung: (1) Rolle annehmen: Ziele, Zwänge, Informationsstand und Kultur des Gegenübers (Wettbewerber, Angreifer, Reviewer, Nutzer) – nicht die eigenen; (2) „Wie würde ich das eigene Vorhaben schlagen/brechen?" – drei konkrete Angriffe, legal und plausibel; (3) stärkste Gegenthese in 5 Sätzen (Devil's Advocate, auch wenn man sie nicht glaubt); (4) Prüfung auf Gruppendenken: Welche Position hat im Team niemand vertreten?; (5) Hall: Kritik ohne Vorschlag reicht nicht – je Einwand eine nicht eigennützige Gegenmaßnahme; (6) Rückgabe als `dissent[]` + `alternative_hypotheses`; (7) Einsatz: als Kind-Agent über `agent_tool` mit **engerer** Kontextdecke (sieht Schlussfolgerung und Belege, nicht die Deliberation des Autors – vermeidet Anchoring).

#### `harw-home/assets/skills/indicators-warnings/skill.toml`

```toml
name = "indicators-warnings"
description = "Szenarien und Annahmen in beobachtbare Indikatoren mit Basislinie, Schwelle, Quelle und Prüftakt übersetzen."
instructions_file = "instructions.md"
tools = ["filesystem.read", "web.search", "web.fetch"]
```

Gliederung: (1) Eingang: Szenario, Hypothese, Annahme oder AAR-Empfehlung; (2) Zerlegung nach Hall: „wenn das eintritt, sehe ich zuerst …" – Aktivitäten, Transaktionen, technische Spuren, **und** das Ausbleiben normaler Aktivitäten; (3) je Indikator: Basislinie (normal), Schwelle (Warnung), Quelle/Sammler, Prüftakt, Vorlaufzeit; (4) Diagnostizität: unterscheidet der Indikator zwischen Szenarien, oder feuert er bei allen?; (5) schwache Signale (Oriesek/Schwarz, Frühwarnkapitel): gezieltes Scanning in den Suchfeldern aus dem Wargame; (6) Rückgabe `indicators[]`; optional Anlage als Kanban-Karte mit Wiedervorlage.

#### `harw-home/assets/skills/source-grading/skill.toml`

```toml
name = "source-grading"
description = "Belege nach Quellzuverlässigkeit (A–F) und Informationsglaubwürdigkeit (1–6) bewerten und Abhängigkeiten zwischen Quellen erkennen."
instructions_file = "instructions.md"
tools = ["filesystem.read", "web.fetch"]
```

Gliederung: (1) Zwei Achsen strikt trennen: *Wer* sagt es (Track Record, Nähe zur Sache, Interessenlage) vs. *Was* wird gesagt (durch Unabhängiges bestätigt? plausibel? widerspruchsfrei?); (2) Zuordnung für unsere Quellklassen: `Standard`/`OfficialDocs`/`CargoRegistrySource` → typ. A–B, `ReleaseNotes`/`Repository` → B–C, `Web` → C–F; lokaler Quelltext ist Primärbeleg (A) für „was der Code tut", nicht für „was er tun soll"; (3) Zirkelbestätigung erkennen: zwei Blogposts, die dieselbe Pressemitteilung zitieren, sind *eine* Quelle (`derived_from`); (4) Täuschung/Veraltung prüfen (Datum, Version, Digest); (5) Rückgabe: je `SourceReference` `reliability`, `credibility`, `derived_from`.

#### `harw-home/assets/skills/hypothesis-decomposition/skill.toml`

```toml
name = "hypothesis-decomposition"
description = "Hypothese oder Fragestellung in konkrete, prüfbare Beobachtungen zerlegen und daraus gebundene Sammelaufträge ableiten."
instructions_file = "instructions.md"
tools = ["filesystem.read"]
```

Gliederung: (1) Treiber benennen – Hall unterscheidet Hypothese, Erwartung, Ahnung, Rätsel; alle werden gleich zerlegt; (2) Funktionen, die erfüllt sein müssen, wenn die Hypothese stimmt; (3) je Funktion Beobachtbares (Dateien, Logzeilen, Versionen, Metriken); (4) daraus `ResearchQuestion`s mit `scope`, `expected_output`, `stop_condition` für Explorer-Kinder; (5) Rückkopplung: welche Beobachtung würde die Hypothese *widerlegen*?

#### Agent-Zuordnung

- `harw-home/assets/agents/source-researcher/agent.toml`: `skills = ["dependency-research", "source-grading"]`.
- Neuer Worker `all-source-analyst` (§3.1): `skills = ["analysis-ach", "key-assumptions", "source-grading", "indicators-warnings"]`.
- Neuer Worker `red-teamer`: `skills = ["red-team", "key-assumptions"]`, kleines Kontextbudget.
- Matrix-Spieler (`matrix-player`): `skills = ["red-team"]` im Business-Modus (Coach-Funktion), Market-Spieler: keine.

### 2.3 Änderungen am Recherche-Schema (`harw-research`)

Alle Felder additiv mit `#[serde(default)]` → alte Findings bleiben gültig.

```rust
// harw-research/src/types.rs

/// Zuverlässigkeit der Quelle (Admiralty/NATO-Achse 1).
#[serde(rename_all = "snake_case")]
pub enum SourceReliability { A, B, C, D, E, F }   // F = nicht beurteilbar

/// Glaubwürdigkeit der Information (Achse 2).
pub enum InfoCredibility { Confirmed, ProbablyTrue, PossiblyTrue, Doubtful, Improbable, CannotJudge }

pub struct SourceReference {
    // … bestehende Felder …
    #[serde(default)] pub reliability: Option<SourceReliability>,
    #[serde(default)] pub credibility: Option<InfoCredibility>,
    /// Locator einer anderen Quelle, von der diese abhängt (Zirkelbestätigung).
    #[serde(default)] pub derived_from: Option<String>,
}

/// Geschätzte Wahrscheinlichkeit der Aussage – getrennt von `Confidence`
/// (Belastbarkeit der Analyse).
pub enum Likelihood { AlmostCertain, VeryLikely, Likely, RoughlyEven, Unlikely, VeryUnlikely, Remote }

pub enum HypothesisStatus { Leading, Viable, Weakened, Refuted }
pub struct HypothesisAssessment {
    pub id: String,                  // "H1"
    pub statement: String,
    pub status: HypothesisStatus,
    #[serde(default)] pub consistent_evidence: Vec<usize>,    // Indizes in `evidence`
    #[serde(default)] pub inconsistent_evidence: Vec<usize>,
    #[serde(default)] pub inconsistency_score: f32,
}

pub enum AssumptionStatus { Supported, Caveated, Unsupported }
pub struct KeyAssumption {
    pub id: String,                  // "A1"
    pub statement: String,
    pub status: AssumptionStatus,
    #[serde(default)] pub on_critical_path: bool,
    #[serde(default)] pub rationale: String,
}

pub struct Indicator {
    pub id: String,                  // "I1"
    pub observable: String,
    pub supports: Vec<String>,       // Hypothesen-/Annahmen-IDs
    #[serde(default)] pub baseline: String,
    #[serde(default)] pub threshold: String,
    #[serde(default)] pub check_every: Option<String>,   // "7d", "per-release"
}

pub struct ResearchFinding {
    // … bestehende Felder …
    #[serde(default)] pub likelihood: Option<Likelihood>,
    #[serde(default)] pub confidence_rationale: String,
    #[serde(default)] pub hypotheses: Vec<HypothesisAssessment>,
    #[serde(default)] pub key_assumptions: Vec<KeyAssumption>,
    #[serde(default)] pub indicators: Vec<Indicator>,
    /// Abweichende Einschätzungen (Red Team, zweiter Analyst).
    #[serde(default)] pub dissent: Vec<String>,
}
```

Neue Regeln in `harw-research/src/validate.rs::validate_finding` (zusätzlich zur bestehenden Beleg-Pflicht ab `Medium`):

1. `hypotheses` nicht leer ⇒ mindestens 2 Einträge und genau eine `Leading`; Evidenz-Indizes müssen in `evidence` liegen.
2. `confidence >= High` ⇒ mindestens eine Quelle mit `reliability ∈ {A, B}` **oder** zwei Quellen, die nicht per `derived_from` voneinander abhängen.
3. Eine `KeyAssumption { status: Unsupported, on_critical_path: true }` ⇒ `confidence <= Medium`.
4. `confidence == Verified` ⇒ mindestens eine Quelle mit `credibility == Confirmed`.
5. `confidence >= Medium` ⇒ `confidence_rationale` nicht leer (Warnung statt Fehler in der ersten Stufe, per Feature-Flag scharf schalten).

`FindingBundle` erhält `#[serde(default)] pub requirements_trace: Vec<(QuestionId, Vec<usize>)>` – welche Findings welche Frage beantworten; `coverage_gaps` bleibt. Der JSON-Schema-Export (`harw-research/src/schema.rs`) muss die neuen Felder aufnehmen, damit Kind-Agenten sie im Prompt sehen.

---

## 3. Intelligence-Zyklus im Harness

### 3.1 Abbildung der Phasen

Herman beschreibt den Prozess als Kette aus Sammlung mit Single-Source-Berichten, All-Source-Analyse zu „fertiger" Intelligence und Verteilung an Nutzer; das Volumen schrumpft dabei stark und der Wert je Einheit steigt (sein Bild: Veredelung von Rohöl). Er warnt zugleich, dass der saubere Zyklus eine Idealisierung ist – in der Praxis schieben Produzenten Ergebnisse aktiv an Nutzer, die eher reagieren als bestellen, und Erstverarbeitung steuert die Sammlung direkt nach.

| Phase | Harwness-Rolle | Artefakt | Bestehender Code |
|---|---|---|---|
| Tasking / Requirements | Root-/Kind-Orchestrator | `ResearchQuestion` (Frage, Scope, Stop-Bedingung); Schlüsselfragen = `key_questions` im Wargame | `harw-research/src/types.rs`, `harw-plan` |
| Collection | Explorer-/Source-Researcher-Worker | Rohbelege: `SourceReference` mit Digest | `harw-explorer`, `harw-tool-web`, `harw-tool-fs`, `harw-lens` |
| Processing | derselbe Worker (Herman: Verarbeitung gehört nah an die Sammlung) | normalisiertes `ResearchFinding`, Quellbewertung | `validate.rs::parse_and_validate` |
| Analysis (All-Source) | `all-source-analyst` oder Orchestrator | fusioniertes Finding mit ACH, Annahmen, Indikatoren | neu: Agent-Asset + Skills |
| Dissemination | Rückgabe an Parent, TUI, Knowledge | `ReturnEnvelope`, AAR, Knowledge-Artefakt | `return_envelope.rs`, `harw-knowledge` |
| Feedback / Re-Tasking | Orchestrator | neue `ResearchQuestion`s aus `unresolved_questions`, `coverage_gaps`, Indikatoren | `FindingBundle.coverage_gaps` |

Konsequenzen:

- **Trennung Single-Source/All-Source**: Worker ziehen Schlüsse nur über ihre eigene Quelle („in `Cargo.lock` steht X"), die übergreifende Bewertung macht der Analyst. Prompt-Regel im Worker-`system.md`: *keine Gesamturteile jenseits des eigenen Scopes*; Schema-Regel: Worker-Findings ohne `hypotheses`, Analyst-Findings mit.
- **Trichter statt Durchreichen**: Kinder geben kompakte Findings zurück, keine Rohdaten; Rohdaten bleiben per `FragmentReference` (Digest) abrufbar. Das ist zugleich Token-Ökonomie (`docs/design/token-efficiency.md`).
- **Push-Modell ernst nehmen**: Worker dürfen `suggested_next_actions` und unerwartete Nebenbefunde melden (eigenes Feld `serendipity: Vec<String>` im `ReturnEnvelope`, optional), statt nur die bestellte Frage zu beantworten.
- **Sammlungssteuerung durch Verarbeitung**: `hypothesis-decomposition` erzeugt Folgefragen direkt; der Orchestrator entscheidet nur über Budget/Admission.

### 3.2 Need-to-know als Prinzip

Herman definiert Need-to-know sinngemäß so, dass Information nur an diejenigen geht, die sie zwingend brauchen – nicht an alle, denen sie nützlich sein könnte. Er warnt aber auch vor überzogener Geheimhaltung, die Nutzung behindert und zum Revierverhalten wird. Beides übernehmen wir:

**Kind-Agenten-Kontext** (existiert bereits im Kern, hier als Leitlinie):
- `ContextCeiling::intersect` kann nur verengen (`harw-context/src/ceiling.rs`) – Need-to-know ist damit strukturell erzwungen.
- Neu als Konvention: Jeder Delegationsauftrag trägt einen `need_to_know`-Abschnitt: welche Sektionen das Kind *braucht*; Default ist Frage + Scope + minimal nötige Fragmente, nicht die Elternhistorie.
- Gegen Überkompartimentierung: **Tearline-Zusammenfassungen** – der Parent darf einem Kind statt des vollen Fragments eine bereinigte Kurzfassung + `FragmentReference` geben; das Kind kann den Volltext nur über eine auditierte Anfrage nachladen, sofern die Decke es erlaubt.
- Red-Team-Kinder bekommen bewusst *weniger* (keine Deliberation des Autors), damit sie unabhängig urteilen.

### 3.3 Compartmentation im Matrix Game

| Informationsklasse | Sichtbar für | Mechanismus |
|---|---|---|
| Gemeinsames Gamebook, `public_situation`, Plenum, offengelegte Ereignisse | alle | öffentlicher Kanal |
| `strategy_brief`, `team_report`, eigene Vorlage | eigenes Team | Team-Scope |
| Paarkanal A↔B | A, B, GM (Mitleser) | privater Kanal mit `observer = gamemaster` |
| Modellinterna, Seed, Inject-Katalog, Adjudikator-Begründungen | nur GM | GM-Scope; nach Endex optional im AAR |
| Marktbewertungen (Rohwerte) | Markt, GM | aggregiert als Anteile öffentlich |

Regeln:
1. **Kein Informationsabfluss durch Zusammenfassung**: Wenn der GM für Team C einen Lagebericht erzeugt, bekommt der erzeugende LLM-Aufruf nur Eingaben, die C sehen darf (Kontextdecke je Empfänger, gleiche Mechanik wie §3.2). Das ist die wichtigste technische Invariante; Test: Kanarienwort in Kanal A↔B darf in keinem Bericht an C auftauchen.
2. **Quellenschutz-Regel** (in Anlehnung an die von Herman geschilderte Praxis, auf geschützte Quellen nur mit anderweitiger Deckung zu handeln): Ein Team darf im öffentlichen Pitch keine Information zitieren, die es nur aus einem privaten Kanal kennt, ohne die Quelle preiszugeben; der GM markiert Verstöße im AAR (nicht blockierend).
3. **Täuschung ist erlaubt** in Paarkanälen (Wettbewerber dürfen bluffen); der GM protokolliert, bewertet aber nicht moralisch. Das Marktteam ist davon ausgenommen (bewertet ehrlich).

---

## 4. Visualisierung nach Zelazny – TUI und Berichte

### 4.1 Grundsätze

Zelaznys Kette: erst die **Botschaft** bestimmen, daraus die **Vergleichsart**, daraus die **Diagrammform**. Der Titel einer Grafik ist die Botschaft, nicht das Thema („Explorer-2 verbraucht zwei Drittel der Tokens" statt „Token-Verbrauch nach Agent"). Er unterscheidet fünf Vergleiche: Anteil am Ganzen (Komponente), Rangfolge (Item), Veränderung über Zeit (Zeitreihe), Verteilung über Größenklassen (Häufigkeit), Zusammenhang zweier Variablen (Korrelation). Kreisdiagramme hält er für überbewertet, Balken für unterschätzt.

Übertragen auf ratatui 0.29 (`harw-tui/Cargo.toml`):

| Vergleich | Signalwörter in der Botschaft | Zelazny-Form | ratatui |
|---|---|---|---|
| Komponente | Anteil, % von, entfällt auf | Kreis / 100-%-Balken | gestapelter 100-%-Balken als eine Zeile farbiger `Span`s; oder `Gauge`/`LineGauge` je Teil |
| Item | größer, kleiner, Rang, gleichauf | horizontale Balken | `BarChart` mit `.direction(Direction::Horizontal)`, absteigend sortiert |
| Zeitreihe | steigt, fällt, schwankt, seit | Säulen / Linie | `Sparkline` (kompakt), `Chart` + `Dataset` `GraphType::Line` |
| Häufigkeit | Bereich, Konzentration, Verteilung | Säulen-Histogramm | `BarChart` vertikal, Buckets als Labels |
| Korrelation | hängt ab von, steigt mit | Punktdiagramm | `Chart` mit `GraphType::Scatter`, `Marker::Braille` |
| exakte Werte | – | Tabelle | `Table` (Zahlen rechtsbündig, Botschaft im Block-Titel) |

### 4.2 Konkrete Anwendungen

**Token-Verbrauch** (`AgentMonitor::totals`, `harw_types::TokenUsage`)
- Komponente: „Explorer-Kinder verbrauchen 58 % der Eingabetokens" – eine 100-%-Zeile pro Lauf (Root / Kinder / UIA-Worker), Cache-Anteil als eigene Komponente („41 % aus Cache").
- Item: „source-researcher ist der teuerste Agent" – horizontaler `BarChart`, Top-8.
- Zeitreihe: `Sparkline` Tokens/Minute je Agent im Agentenpanel (eine Zeile Höhe).
- Gauge: Kontextbelegung gegen `ContextBudgetSpec` – `Gauge` mit Titel „Kontext 82 % – Compaction bald fällig".

**Agentenaktivität**
- Häufigkeit: „Die meisten Tool-Aufrufe dauern unter 2 s, fünf über 30 s" – Histogramm der Tool-Latenzen.
- Item: Tool-Aufrufe je Werkzeug, horizontal.
- Korrelation: „Längere Kontexte gehen nicht mit mehr Retries einher" – Scatter Kontextgröße × Retries je Turn (nur im Report/Detailansicht, nicht im Live-Panel).

**Matrix Game – Weltzustand** (neues Panel, Tab `Lage`)
- Komponente: Marktanteil je Segment im aktuellen Zug (100-%-Zeile je Segment, Teamfarben).
- Zeitreihe: Anteil/EBIT je Team über Züge – `Chart` Line, x = Zug-Labels.
- Item: Rangfolge Cash nach Zug.
- `Table`: KPI-Matrix Team × KPI mit Δ zum Vorzug; Titel automatisch: größte Veränderung.
- Liste (kein Diagramm): Ereignis-Zeitleiste (Injects, Offenlegungen) mit Zug/Phase.
- Sichtbarkeit: Die TUI zeigt für einen Spieler-Blick nur dessen Scope (gleiche Compartment-Regeln wie §3.3); der Spielleiter-Blick zeigt alles.

**AAR-Berichte** (Markdown, `harw-tui/src/export.rs` bzw. AAR-Renderer)
- Jede Sektion beginnt mit einer Botschaftsüberschrift; pro Botschaft höchstens ein Diagramm.
- Markdown-Diagramme als Unicode-Balken (`█▉▊▋▌▍▎▏`) + Tabelle mit Zahlen; kein Kreis.
- Prognosefehler (erwartete vs. tatsächliche KPIs) als paarweise Balken je Team (Zelazny nutzt gepaarte Balken auch für Zusammenhänge mit wenigen Punkten).

### 4.3 Botschaftstitel automatisch erzeugen

Neues Modul `harw-tui/src/charts.rs` (rein, testbar):

```rust
pub enum Comparison { Component, Item, TimeSeries, Frequency, Correlation }

/// Leitet aus Daten eine Botschaft ab; Fallback ist der Thementitel.
pub fn message_title(kind: Comparison, series: &LabeledSeries, topic: &str) -> String;
// Component: größter Anteil ≥ 40 % → "{label} macht {pct} % aus"
// Item: Abstand Platz1/Platz2 ≥ 1.5× → "{label} führt deutlich"; sonst "… etwa gleichauf"
// TimeSeries: Steigung/Varianz → "steigt seit …" | "fällt" | "schwankt"
// Frequency: Modus-Bucket → "Meiste Werte zwischen {a} und {b}"
// Correlation: |r| < 0.2 → "Kein Zusammenhang zwischen …"; sonst Richtung
```

Dieselbe Funktion nutzen AAR-Renderer und TUI, damit Report und Bildschirm dieselbe Aussage treffen.

---

## 5. Priorisierte Umsetzungsliste

| Prio | Arbeitspaket | Dateien / Crates | Abnahme |
|---|---|---|---|
| P0 | Recherche-Schema additiv erweitern (Quellbewertung, Likelihood, Hypothesen, Annahmen, Indikatoren, Dissent) | `harw-research/src/types.rs`, `schema.rs`, `validate.rs` | Alt-Findings deserialisieren unverändert; Regeln 1–4 aus §2.3 mit Tests |
| P0 | Sechs Skills anlegen | `harw-home/assets/skills/{analysis-ach,key-assumptions,red-team,indicators-warnings,source-grading,hypothesis-decomposition}/{skill.toml,instructions.md}` | `harw-config` Discovery lädt sie (`discovery.rs`), `deny_unknown_fields` grün |
| P0 | Szenario-Schema `mode = "business"` + Validierung | Matrix-Crate laut `matrix-game.md` (Szenario-Parser), Beispiel `cloud-sme-2027` als Fixture | Beispiel aus §1.9 lädt; Negativtests für jede Validierungsregel |
| P1 | GM-Business-Phasen, Entscheidungsvorlage `submit_move`, Regelprüfung, Logit-Marktmodell, gebundene Overrides | Matrix-Crate (`gamemaster`, `business/market_model.rs`, `business/rules.rs`) | deterministischer Replay bei gleichem Seed; Override außerhalb Bounds wird abgelehnt |
| P1 | Compartment-Invariante: Berichte je Empfänger nur aus dessen Scope, GM als Mitleser, Offenlegungsregeln | Matrix-Crate + `harw-context` (Decke je Empfänger) | Kanarienwort-Test (§3.3 Regel 1) |
| P1 | Agent-Assets `all-source-analyst`, `red-teamer`; `source-researcher` um `source-grading` ergänzen; Worker-Prompt „kein Urteil jenseits des Scopes" | `harw-home/assets/agents/*` | Assets laden; Delegationsmatrix erlaubt Orchestrator → neue Worker |
| P1 | AAR `harwness.matrix-aar/v1`: Kanalanalyse, Prognosefehler, Bias-Checkliste, `CounterApproach`, Indikator-Export | Matrix-Crate (`aar.rs`), Ablage über `harw-knowledge` | AAR beantwortet jede `key_question` mit Beleg-IDs |
| P2 | `charts.rs` mit `Comparison` + `message_title`; Token-Komponentenzeile, Sparklines, Kontext-Gauge im Agentenpanel | `harw-tui/src/charts.rs`, `agent_monitor.rs` | Snapshot-Tests der Render-Ausgabe; Titel-Heuristik-Unit-Tests |
| P2 | TUI-Tab „Lage" für Matrix Game (KPI-Table, Anteilszeilen, Zeitreihe, Ereignisliste) mit Scope-Filter | `harw-tui/src/` (neues Panel), `panes.rs` | Spieler-Blick zeigt keine fremden Kanäle |
| P2 | Tearline-Zusammenfassungen + `need_to_know`-Abschnitt im Delegationsauftrag | `harw-core/src/child_controller.rs`, `harw-context/src/reference.rs` | Kind erhält Referenz statt Volltext; Nachladen auditiert |
| P3 | Variante `attack_counter` mit Botschafter-Briefs | Matrix-Crate | Drei-Runden-Ablauf spielbar |
| P3 | Indikatoren als Wiedervorlage (Frühwarnung) | `harw-knowledge` (kanban), Skill `indicators-warnings` | Indikator aus AAR erscheint als Karte mit Prüftakt |
| P3 | `ReturnEnvelope.serendipity` (Push-Befunde) | `harw-research/src/return_envelope.rs` | optionales Feld, abwärtskompatibel |

Offene Punkte: Name und Struktur der Matrix-Crate sowie die exakten Kern-Felder (`[[teams]]`, `[channels]`, `[[injects]]`) sind mit `matrix-game.md` abzugleichen; ob der LLM-Adjudikator standardmäßig aktiv ist, entscheidet das Kostenbudget je Spiel.
