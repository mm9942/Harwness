# Harwness Ausbauprogramm AW0–AW7 — korrigierter Ausführungsplan

## Kontext

Im Paket `files.zip` liegen fünfzehn Planungsdokumente (315 KB) für ein
Ausbauprogramm, das Harwness um fünf Teilsysteme erweitert: **DoD**
(Sicherheitssensorik, Regeln, Durchsetzer), **Kontext** (Programme, Ceiling,
Vertrauensklassen), **Lens** (Retrieval, Index, Föderation), **Telemetrie**
(Sinks, Routing) und eine **Control Plane** (Next.js über `harw-web`).

Das Programm setzt ausdrücklich auf der gerade abgeschlossenen Plan-/Goal-
Integration auf: „Scheduling, Revier, Ausführung, Rückgabe und Budgets
existieren bereits. Kein Teilsystem baut einen eigenen Scheduler, ein eigenes
Fan-out oder einen eigenen Rückgabeweg."

**Warum dieser Plan und nicht die Dokumente selbst.** Eine Bottom-up-Analyse
über drei Ebenen hat die Dokumente gegen den Ist-Code geprüft. Ergebnis: die
Fundamente, auf die sich das Programm beruft, existieren fast durchweg und
exakt wie beschrieben — die Autoren haben sorgfältig recherchiert. Aber es
gibt **zweiundzwanzig Befunde**, die vor der Umsetzung aufzulösen sind. Der
schwerwiegendste war in keinem der Dokumente sichtbar: **die
Root-`Cargo.toml` zählt ihre Member explizit auf, das Programm legt rund
fünfzig neue Crates an, und dieser Pfad steht in keinem einzigen
Schreibbereich.** Elf parallele Sensor-Agenten hätten elf gleichzeitige
Änderungen an derselben Liste erzeugt.

Dieser Plan übernimmt das Programm, korrigiert die zweiundzwanzig Punkte mit
ausgeschriebener Begründung und macht daraus eine ausführbare Wellenfolge von
**93 Knoten in 29 Ausführungsebenen**.

**Nutzerentscheidungen (2026-09-01):** Umfang = ganzes Programm AW0–AW7 plus
UI. Umgang mit Widersprüchen = korrigieren und begründen, wo der Ist-Code die
bessere Lösung bereits hat.

**Umbenennung der Wellen.** Das *vorherige* Programm benutzte dieselbe
Nomenklatur; `git log` enthält `W5a`, `W5b`, `W7:`, `W8:`. Ein Commit
„W2-07: harw-dod-thermal" wäre davon nicht unterscheidbar, und
`git log --grep="W7"` liefert beides. Dieses Programm heißt daher **AW0–AW7**
(Ausbau-Welle), Knoten `AW2-07`. Kostenlos, und es hält Historie und
Knoten-IDs eindeutig.

---

## Ist-Stand, nachgezählt

| Kennzahl | Programm sagt | Nachgemessen | Befund |
|---|---|---|---|
| Workspace-Member | 46 | **46** | exakt |
| Interne Kanten | 205 | **207–215** | gewachsen (Zählweise mit/ohne dev-deps) |
| Tiefe | 8 | **8** | exakt |
| Zeilen Rust | ~192.000 | **188.521** (`*/src/**`) / 195.818 (alle `.rs`) | im Rahmen |
| Tests | 3151 | **3239** | +88 seit dem Snapshot |
| Neue Crates | „24 plus Fassade" (DoD) | **~50 gesamt**, davon 27 DoD-Libs + 4 Binaries | Zahl nicht nachgezählt |
| Arbeitsknoten | 79 | **72 aufgezählt**, nach Korrektur **93** | Zahl nicht rekonstruierbar |

Die Dokumente wurden am 2026-09-01 gepackt, der letzte Commit davor datiert
auf 2026-08-27. Die Drift erklärt die Abweichungen; sie bestätigt die eigene
Warnung des Implementation Guide: „Der Baum bewegt sich."

**Keiner der ~50 geplanten Crate-Namen kollidiert mit einem bestehenden
Verzeichnis.** Das Programm baut auf freiem Namensraum auf.

---

## Die Korrekturen

Jede nennt: was das Programm sagt, was der Code sagt, was gilt.

### A — Widersprüche zum Ist-Code

**K1 · `EgressSet` wird nicht gebaut; `NetworkScope` wird erweitert.**
Das Programm sagt, `Permission::NetworkAccess` sei „ein Schalter" und brauche
daneben eine neue Mengensemantik. Der Code sagt etwas anderes:
`harw-sandbox/src/lib.rs:144` führt `NetworkScope { allow_hosts:
BTreeSet<String> }` mit exakt der geforderten Halbverbands-Disziplin — kein
`add`, kein `union`, nur `intersection`; Normalisierung in `from_hosts`
(Trim, ASCII-Lowercase, führende Punkte weg, Leereinträge verworfen);
`allows` case-insensitiv mit Punktgrenzen-Schutz gegen `evildocs.rs`; dazu
`is_subset_of` und `SandboxSpec::restrict_network`. Aktiv verdrahtet in drei
Aufrufstellen (`harw-core/src/session.rs`, `harw-tools/src/sandbox_guard.rs`,
`harw-tool-web/src/fetch.rs`), kein totes Feld.

**Es gilt:** `allow_hosts: BTreeSet<String>` wird zu
`allowed: BTreeSet<EgressTarget>` mit `EgressTarget::{Host, DnsSuffix, Cidr}`.
`from_hosts` bleibt als Konstruktor und erzeugt `Host`-Varianten;
**`allows(&str)` behält Signatur und Semantik unverändert**, neu kommt
`allows_addr(IpAddr)` hinzu. An den drei Aufrufstellen ändert sich damit
zunächst keine Zeile. Ein paralleler `EgressSet` erzeugte zwei Wahrheiten
über dieselbe Frage — genau das, was der Leitfaden §1 („Verdichten statt
vervielfachen") verbietet.

*Folge für die Wellen:* AW3-01 bleibt allein auf `harw-sandbox/**`. Die
Migration der Aufrufstellen auf CIDR-Ziele wird ein **eigener Knoten
(AW4-08)**, gegated auf AW4-01, weil sie `harw-core/**` berührt und dort mit
drei anderen Knoten kollidieren würde. Neue Abhängigkeit `ipnet` (reines
Rust, kein `build.rs`, D3-konform) — Version über `cargo add` auflösen,
nie hardcoden.

**K2 · `harw-observe` ist ein AW0-Knoten, keine Voraussetzung.**
Der Telemetrie-Plan beginnt mit „`harw-observe` definiert bereits
`TelemetrySink` und kennt kein Backend" — im Präsens. Tatsächlich haben
`TelemetrySink`, `MetricKey`, `TraceContext` und `FieldName` **null**
Vorkommen im Baum. Der Plan trägt zwar oben „noch nicht normativ", aber §0
liest sich als Bestandsbeschreibung; wer der Lesereihenfolge des Index folgt,
baut gegen einen nicht existierenden Trait.

**Es gilt:** AW0-01. Und weiter: **`NullSink` allein friert einen Trait nicht
ein.** Die drei Dinge, die an einem Sink-Trait erfahrungsgemäß driften —
Receiver (`&self` vs `&mut self`), Rückgabe (`()` vs `Result`), Sync/Async —
sind gegen `NullSink` alle unauffällig und gegen einen rotierenden
JSONL-Writer alle sichtbar. **`harw-observe-file` wandert von AW1 nach AW0 und
wird mit AW0-01 zu einem Knoten verschmolzen.** Bei rund 41 abhängigen Crates
ist das die billigste Versicherung im ganzen Programm.

**K3 · Konfidenz ist vierfach besetzt, nicht dreifach — und `PalaceConfidence`
existiert nicht.**

| Ist | Ort | Stufen |
|---|---|---|
| `harw_research::Confidence` | `harw-research/src/types.rs:181` | 4: Low/Medium/High/Verified |
| `harw_memory::epistemic::Confidence` | `harw-memory/src/epistemic.rs:127` | 5: VeryLow..VeryHigh |
| `harw_model_catalog::provenance::Confidence` | `harw-model-catalog/src/provenance.rs:203` | 5: VeryLow..VeryHigh |
| `harw_knowledge::memory::palace::Confidence` | `harw-knowledge/src/memory/palace.rs:16` | 3: Established/Provisional/Superseded |

Die Palace-Variante ist **keine Konfidenzskala**, sondern ein Lebenszyklus.
Eine Abbildung „High → Established" ist eine Kategorienverwechslung, keine
Konvertierung. Drei getrennte Festlegungen:

- **Umbenennen:** `harw_knowledge::memory::palace::Confidence` →
  **`PalaceStatus`**. Reine Umbenennung mit Compiler-Unterstützung, gehört in
  AW4-05 (fasst `harw-knowledge` ohnehin an), beseitigt die Verwechslung an
  der Wurzel statt sie in einem Konvertierungssymbol zu verstecken.
- **Zusammenlegen:** `model_catalog::provenance::Confidence` und
  `memory::epistemic::Confidence` sind formgleich. Nach K6 („pro
  Vokabularpaar genau ein Symbol") ein Duplikat, kein Paar. **Geprüft:
  `harw-model-catalog` hängt nur an `harw-config` und `harw-types`,
  `harw-memory` hat null interne Kanten — kein Zyklus.** Trotzdem *nicht*
  Reexport `model-catalog → memory`: das zöge die ganze Memory-Crate für ein
  Enum herein. **Es gilt: der Typ zieht nach `harw-types` (AW0-03), beide
  reexportieren.** Das ist die Crate für querschneidendes Vokabular, und
  `harw-memory` bekommt dabei seine erste interne Kante — auf den tiefsten
  Leaf, also unkritisch.
- **Behalten und benennen:** Nur `research::Confidence → epistemic::Confidence`
  ist eine echte Konvertierung (4 → 5 Stufen). Symbol
  `epistemic_confidence_for`, Ort `harw-dod-rules` (AW4-03).

`Baseline.confidence` ist damit `harw_knowledge::memory::palace::PalaceStatus`.
Ein Alias `PalaceConfidence` wird **nicht** eingeführt.

**K4 · D5 wird präzisiert, nicht gebrochen.**
D5 verlangt „keine Pre-1.0-Abhängigkeit in einem öffentlichen Typ". Derselbe
Plan schreibt `jiff::Timestamp` (0.2.32) in acht neue öffentliche Structs —
und im Ist-Code steht es bereits öffentlich in
`ChildLeaseRecord.admitted_at`/`lease_expires_at`
(`harw-session-store/src/child_lease.rs:27-28`).

**Es gilt:** „Pre-1.0 nie in öffentlichen Signaturen, **außer für die im
Inventar namentlich geführten Fundamentabhängigkeiten**; derzeit genau eine:
`jiff`." Der Sinn von D5 bleibt erhalten: er schützt vor Abhängigkeiten mit
großer Fläche und unkontrollierter Bruchrate (OpenTelemetry-Familie,
versionsgleichgeschaltet, Brüche in Minors). `jiff` ist eine Zeitbibliothek
mit einem relevanten Typ. Das ist ein anderer Risikofall, und die Doktrin
soll das sagen statt es durch eine stillschweigende Ausnahme zu regeln —
sonst wird der Widerspruch bei jedem der ~50 neuen Crates neu ausgetragen.

**K5 · `FreezeStore` erbt die Mechanik, nicht den Typ — und die
Abhängigkeitskante steht falsch herum.**
Der Plan sagt in einem Satz „Semantik aus `ChildLeaseStore` wiederverwendet"
und entwirft `Freeze<S>` als PhantomData-Typestate. Beides zusammen geht
nicht: `ChildLeaseRecord` ist ein schlichter Serde-Struct mit acht Feldern,
**ohne** Typestate. Sein Zustand steht im Dateisuffix
(`.active.json` / `.expired.json` / `.completed.json`), der Übergang ist ein
`std::fs::rename` (Zeile 119) — daher die Single-Delivery-Garantie —, dazu
`fs4`-Advisory-Lock, `sync_data` plus Parent-fsync, `tempfile::persist`,
Symlink-Abwehr über `is_regular_file` (Zeile 106) und Reconciliation vor
jeder anderen Operation. Das Typestate-Vorbild ist `harw-provider/marker.rs`,
eine andere Stelle.

**Es gilt:** Übernommen wird die **Store-Mechanik**. Nicht übernommen wird die
**Schlüsselung**: `ChildLeaseRecord` schlüsselt über `SessionId` und darf
annehmen, dass Child-IDs global eindeutig sind (Zeile 56-57: „an existing
terminal record is also a hard error"). **Cgroup-IDs werden
wiederverwendet.** Ein `FreezeStore`, der die Annahme erbt, hält einen alten
Freeze für einen Konflikt oder — schlimmer — lässt einen neuen Prozess in
einem geerbten Freeze-Datensatz landen. **Verbindlich: `Freeze` wird über
`(CgroupId, FindingId, frozen_at)` geschlüsselt, nie über `CgroupId` allein.**

**Kantenkorrektur:** Der Plan hat `AW5-05 (FreezeStore) hängt an AW5-03
(escalate)`. Die Signatur im Plan selbst ist
`reconcile_expired_freezes(store: &FreezeStore, now)` — escalate *nimmt* den
Store. **Korrigiert: AW5-03 hängt an AW5-05.**

### B — Planungsfehler im Work-Breakdown

**K6 · Sieben Strom-A-Sensoren, nicht acht.**
`[W2-06..W2-13]` ist ein Bereich von acht IDs für sieben aufgezählte Namen
(thermal, cpu, memory, blockio, netcounters, gpu, cgroup); die eigene Quelle
sagt ebenfalls „sieben". Trivial zu korrigieren und trotzdem wichtig: der
Bereich ist die Fan-out-Anweisung für eine elf Agenten breite Zelle. Eine ID
zu viel heißt ein Agent ohne Aufgabe — oder, wahrscheinlicher, ein Agent, der
sich eine achte Sensorquelle ausdenkt.
**Es gilt:** `AW2-07..AW2-13`.

**K7 · AW1 hat eine dreifache Kollision auf `harw-core`, nicht eine doppelte.**
Nicht zwei Knoten schreiben `harw-core`, sondern drei: AW1-03
(`context_budget.rs`, `turn_loop.rs`), AW1-04 (`harw-core/**`) und **AW1-07**
(`harw-observe/**`, `harw-core/**`). Zwischen keinem Paar existiert ein Pfad.
Und **Anhang A führt AW1-07 in der `harw-core`-Zeile gar nicht** — die
Prüftabelle, die Disjunktheit sicherstellen soll, ist genau dort
unvollständig, wo die Kollision sitzt.

**Es gilt: teilen, nicht sequenzieren.** AW1 ist die kritische Welle („W1 vor
allem anderen"); sie zu einer Dreierkette zu machen verdreifacht die Laufzeit
an der teuersten Stelle. Und die Teilung ist inhaltlich sauber:

- **AW1-03** behält `context_budget.rs` + `turn_loop.rs` exklusiv und
  **übernimmt die zugehörigen Metriken selbst** (`ContextAssembly`,
  `TokenUsage`). Eine Metrik über eine Montage zu emittieren, die man im
  selben Atemzug neu schreibt, ist ein Stück Arbeit, keine zwei.
- **AW1-04** schrumpft auf `child_controller.rs` + `session.rs` und hängt an
  AW1-03 — **nicht** wegen der Dateien, die sind disjunkt, sondern wegen
  `harw-core/src/lib.rs` (Reexporte) und `harw-core/Cargo.toml` (neue
  `harw-observe`-Abhängigkeit). Zwei Agenten, die dieselbe `Cargo.toml`-Zeile
  hinzufügen, ist der billigste vermeidbare Merge-Konflikt im Programm.
- **AW1-04b** (neu) nimmt `harw-memory/src/context_selector.rs` +
  `harw-tools/src/executor.rs` — vollständig disjunkt, läuft parallel.
- **AW1-07** verliert `harw-core/**` ganz und behält die
  Nullzähler-*Mechanik* in `harw-observe/**`. Die *konkreten* Nullzähler
  wandern zu den Knoten, die die jeweilige Invariante einführen —
  `trust_block_violation` zu AW4-01, die drei `steward_*` zu AW6-08,
  `lens_remote_embed_on_operator_only` zu AW7-05. **Das ist zusätzlich eine
  Designkorrektur:** ein Knoten, der Zähler für Invarianten registriert, die
  es noch nicht gibt, kann sie nicht auf null prüfen.

**K8 · AW6 hat zwei unverbundene Cluster — der `harw-agent-dsl`-Cluster wird
durch Verlagerung aufgelöst, nicht durch Sequenzierung.**
`harw-agent-dsl/**` wird von AW6-01, AW6-03, AW6-05, AW6-08 **und AW7-06**
geschrieben, in zwei unverbundenen Clustern. `harw-knowledge/**` von AW6-06
und AW6-08 ohne Pfad dazwischen.

**`harw-agent-dsl`: verlagern.** Der Inhalt von AW6-03, AW6-05, AW6-08 und
AW7-06 ist dort **Daten, nicht Code** — Definitionen (Familie, Rollen,
Kontextprogramme). Solche Definitionen liegen im Baum bereits unter
`harw-registry-defaults/agents/**`. Also schreibt nur **AW6-01** noch
`harw-agent-dsl/src/**`; die übrigen schreiben je ein eigenes Verzeichnis
unter `harw-registry-defaults/agents/`.

Das verschiebt die Kollision aber nur eine Ebene tiefer, denn
`harw-registry-defaults/src/embedded_agents.rs` zählt die eingebetteten
Definitionen **explizit per `include_str!` auf** (nachgeprüft: Zeilen
114-125, 274-278, 301). Deshalb: **AW6-00 (neu) — Registry-Manifest.**
`harw-registry-defaults` liest sein Inventar aus einer Verzeichniskonvention
(`include_dir!` oder `agents/manifest.toml`) statt aus einer Rust-Liste.
Danach ist „eine Definition hinzufügen" = „eine Datei anlegen", nie „eine
geteilte Datei ändern". Dieselbe Bewegung wie AW0-00, und der eigentliche
Hebel: **die Parallelität echt machen, statt sie zu behaupten.**

**`harw-knowledge`: sequenzieren.** Hier hilft Teilen nicht — AW6-06
(`ObservedModelBehavior`-Vorschläge als durabler Dream-Job) und AW6-08
(Context Steward) bauen beide auf derselben Vorschlags-/Artefaktmechanik auf,
die AW5-09 (`ContextProposal`) einführt. Sie zu trennen hieße, die Mechanik
zweimal zu bauen. **Kante: AW6-08 hängt an AW6-06.** Vollständige Kette:
AW3-03 → AW4-05 → AW5-09 → AW6-06 → AW6-08.

**K9 · Anhang A ist an sechzehn Stellen unvollständig.**
Er nennt sechs Crates. Es fehlen: `harw-core`→AW1-07, `harw-knowledge`→AW3-03,
und ganz: `harw-context` (AW0-04, AW5-07, AW6-07), `harw-extension-api`,
`harw-lens-query`, `harw-lens-source`, `harw-lens-embed`, `harw-dod-signals`,
`harw-dod-escalate`, `harw-dod-rules`, `harw-registry-defaults`,
`harw-session-store`, `harw-memory`, `harw-model-catalog`, `harw-tools`,
`xtask` (über Trackgrenze hinweg) und die **Root-`Cargo.toml`**.

Anhang A ist als *Prüfinstrument* deklariert. Ein unvollständiges
Prüfinstrument ist schlechter als keins, weil es Sicherheit suggeriert.
**Es gilt:** Anhang A wird nicht von Hand gepflegt, sondern von AW0-10 aus den
`Schreibbereich`-Zeilen **generiert** und als CI-Gate gegen die `Hängt
an`-Kanten geprüft. Genau die Sorte Regel, die der Leitfaden §1 fordert:
unausdrückbar machen statt prüfen.

### C — Strukturfehler, in den Dokumenten nicht sichtbar

**K10 · Die Root-`Cargo.toml` ist der meistumkämpfte Pfad und steht in keinem
Schreibbereich.**
Die `members`-Liste ist eine **explizite Aufzählung ohne Globs** (46
Einträge, nachgeprüft). Das Programm legt ~50 neue Crates an. Jeder dieser
Knoten müsste sie ändern. Bei elf parallelen Sensor-Agenten in AW2 heißt das:
elf gleichzeitige Änderungen an derselben Liste — entweder Merge-Konflikt
oder, im Fan-out mit `write_partition = Required`, eine Crate, die still
verlorengeht und erst beim nächsten `cargo build` als „unresolved import"
auftaucht. Dazu `Cargo.lock`, den jeder Knoten mit neuer Abhängigkeit anfasst.

**Es gilt: AW0-00 (neu).** Ein Knoten am absoluten Anfang legt **alle ~50
Crate-Verzeichnisse als Stubs an** (`Cargo.toml` + `src/lib.rs` mit
`//!`-Doc), trägt sie geschlossen in `members` ein und richtet
`[workspace.dependencies]` und `[workspace.lints]` ein. Danach hat kein
anderer Knoten die Root-`Cargo.toml` im Schreibbereich. **Das ist die
wichtigste einzelne Strukturkorrektur des Programms, weil sie die
Parallelität überhaupt erst zulässt.**

**K11 · `harw-lens-rank::pack` widerspricht `harw-context` — in derselben
Welle, ohne Kante dazwischen.**
`pack(candidates: &[Ranked], cost: &dyn CostEstimator, budget: &BudgetSpec)
-> Packed` steht in AW0-09 (`harw-lens-rank`, hängt nur an AW0-08). Aber
`CostEstimator` und `BudgetSpec` werden von **AW0-04 (`harw-context`)**
definiert. Und die Architektur zeichnet die Kante `harw-context →
harw-lens-rank`. Also: AW0-09 braucht Typen, die AW0-04 definiert, während
AW0-04 die Crate braucht, die AW0-09 baut. **Ein Zyklus zwischen zwei Knoten
derselben Welle, beide ohne Kante zueinander** — und AW6-07s Abnahme
(„Assembly ruft dieselbe `pack`-Funktion wie Lens") macht daraus einen echten
Cross-Subsystem-Vertrag.

**Es gilt:** `Ranked`, `BudgetSpec`, `trait CostEstimator`, `EdgeIndex`,
`CollapsePolicy`, `Packed` wandern nach **`harw-lens-types` (AW0-08)**.
`harw-lens-rank` (AW0-09) enthält nur die vier Funktionen.
`harw-context::ContextBudgetSpec` wird ein Newtype über
`harw_lens_types::BudgetSpec`. Damit hängen AW0-09 **und** AW0-04 an AW0-08
und laufen parallel. Das ist zugleich die Antwort auf die offene Entscheidung
§8.7 („Wo `harw-lens-rank` wohnt"): **Typen ins Typ-Crate, Funktionen ins
Funktions-Crate** — und die Entscheidung ist ein AW0-Blocker, kein Vorschlag
für später.

**K12 · `Finding<Raw>`s Konstruktor ist so nicht baubar.**
AW0-07 fordert „Konstruktoren `pub(crate)` in der jeweils besitzenden Crate".
Wenn `Finding<S>` in `harw-dod-signals` lebt, ist ein `pub(crate)`-Konstruktor
dort für die elf Sensor-Crates **nicht erreichbar**. Das Typestate-Muster aus
`harw-provider/marker.rs` funktioniert, weil Typbesitzer und Übergangsbesitzer
dieselbe Crate sind. Hier sind sie es nicht.

**Das ist die gefährlichste Vertragslücke des Programms, weil sie exakt im
elf Agenten breiten Fan-out von AW2 aufschlägt — alle elf scheitern
gleichzeitig am selben Fehler.**

**Es gilt:** Sensoren minten **nie** ein `Finding`. Sie emittieren
`HostSample` und `SecurityEvent`. `Finding<Raw>` und `Finding<RuleChecked>`
entstehen ausschließlich in **`harw-dod-rules` (AW4-03)**, `Triaged` in
`harw-dod-escalate` (AW5-03). Das deckt sich mit C6 („`harw-dod-sentinel`
enthält keine Parselogik") und S1 (Autorisierung entsteht an genau einem Ort)
und macht die Sensor-Schnittstelle so schmal wie möglich: ein Trait, zwei
Ausgabetypen, kein Typestate.
*Verworfene Alternative:* ein Sealed-Token, das `harw-dod-signals` an
Sensoren aushändigt — funktioniert, verteilt die Konstruktionsfähigkeit aber
auf elf Crates und macht S1 wieder zu einem Reviewpunkt statt zu einem
Sichtbarkeitsproblem.

**K13 · Sechs Crates der Architektur haben keinen Arbeitsknoten.**
Jede in der Architektur genannte Crate gegen die Schreibbereiche geprüft:

| Crate | Ebene | Folge des Fehlens |
|---|---|---|
| `harw-dod-netlink` | L2 | blockiert AW2-15 (`authlog` braucht `AuditNetlink`) |
| `harw-sentinel` | Binary | AW2-18s Abnahme „erstes lauffähiges Binary" wird von einem Bibliotheksknoten behauptet |
| `harw-dod` | Fassade L5 | in fünf Architekturabschnitten genannt, nie gebaut |
| `harw-lens` | Fassade L4 | dito |
| `harw-tool-lens` | L5 | **Lens wird über elf Crates und fünf Wellen gebaut und erreicht nie einen Agenten** |
| `harw-home` | — | Sicherheitsplan §11 verlangt Pfade für Reports-Dir, FreezeStore, Ring-Snapshots |

**Es gilt:** sechs neue Knoten — AW0-11 (`harw-home`), AW2-04
(`harw-dod-netlink`), AW2-19 (`harw-sentinel`), AW5-10 (`harw-lens`-Fassade),
AW6-09 (`harw-dod`-Fassade), AW6-10 (`harw-tool-lens`).

**K14 · `xtask` wird von zwei Knoten gleichzeitig angelegt.**
Weder `xtask`, `ci`, `deploy` noch `webui` existieren; gebaut wird über ein
22 KB großes `Makefile`. AW0-10 schreibt `ci/**, xtask/**`, UI-01 schreibt
`webui/**, xtask/**` — und der Index sagt ausdrücklich, UI0–UI3 „können
jederzeit gebaut werden", also potenziell gleichzeitig.
**Es gilt:** `xtask` wird in AW0-00 als Stub angelegt; `xtask/src/main.rs`
gehört AW0-00, `xtask/src/gates.rs` gehört AW0-10, `xtask/src/webui.rs`
gehört UI-01. `ci/**` wird zu `Makefile`-Targets plus `xtask`-Unterbefehlen
(Begründung unter „Offene Entscheidungen", Nr. 2).

**K15 · `Provenance` verletzt die Eigentumskarte.**
Die Architektur weist `Provenance` an `harw-memory` zu (nachgeprüft:
`harw-memory/src/epistemic.rs:79`, ein `enum`). Der Kontextplan definiert ein
`struct Provenance { source: SourceId, produced_at: jiff::Timestamp }` in
`harw-context`. Nach dem eigenen Verfahren („Prüfen, ob der Begriff schon
einen Eigentümer hat. Wenn ja, importieren statt neu definieren") ist das ein
Verstoß. Die Typen sind auch inhaltlich verschieden: der eine sagt „woher
weiß ich das", der andere „welcher Provider hat das wann geliefert".
**Es gilt:** `harw_context::FragmentOrigin` statt `Provenance`.

**K16 · `jiff` steht in drei Versionen im Baum, ohne `[workspace.dependencies]`.**
`0.2` / `0.2.28` (×3) / `0.2.32` (×11), und es gibt **kein**
`[workspace.dependencies]` (nachgeprüft). Bei ~50 neuen Crates, die
„ausnahmslos `jiff`" sein sollen, ist das kein Schönheitsfehler mehr, sondern
ein Multiplikator. **Es gilt:** eine Version in `[workspace.dependencies]`,
gesetzt in AW0-00.

### D — Veraltete Bestandsangaben

**K17 · `#![forbid(unsafe_code)]` fehlt in 8 von 45 Crates — und alles echte
`unsafe` ist testgekapselt.**
Fehlend in `harw-browser`, `harw-browser-thirtyfour`, `harw-channel-browser`,
`harw-core-bridge`, `harw-macros`, `harw-memory`, `harw-secrets`,
`harw-tool-browser`. Kein `[workspace.lints]`. **Präzisierung gegenüber der
ersten Zählung:** die drei echten `unsafe`-Stellen liegen alle hinter
`#[cfg(test)]` — `harw-secrets/src/kek.rs:368,376,381` (`std::env::set_var`)
und `harw-browser/src/session.rs:145,149` sowie `host.rs:409` (No-op-Waker).
**Im Produktivcode des Workspaces gibt es kein `unsafe`.**

**Es gilt:** `[workspace.lints.rust] unsafe_code = "forbid"` in der
Root-`Cargo.toml` plus `[lints] workspace = true` in allen 45 bestehenden und
allen ~50 neuen Crates (AW0-00). Eine Regel, die pro Crate wiederholt werden
muss, wurde in 8 von 45 Fällen vergessen und würde bei 96 Crates entsprechend
öfter vergessen. Die drei Testhelfer werden ersetzt: `set_var` durch
Serialisierung über `std::sync::Mutex`, die zwei Waker durch
`std::task::Waker::noop()` (stabil seit 1.85; Edition 2024 setzt das ohnehin
voraus). Das eigentliche Risiko ist nicht das vorhandene `unsafe`, sondern
dass der Leitfaden die Invariante als geltend behauptet — ein Agent liest sie
und nimmt an, sie sei durchgesetzt.

**K18 · `harw-macros` hat 20 externe Konsumenten, nicht elf — und ist die
längste Serialkette des Programms.**
21 `Cargo.toml` nennen es (inklusive der eigenen). Die Warnung „ein Fehler
dort verteilt sich über den halben Workspace" ist damit doppelt so scharf wie
dokumentiert. Zugleich ist es vier Wellen lang alleinbesetzt: AW0-02
(`metrics!`, `field!`, `Redact`) → AW1-02 (`#[traced]`) → AW2-06
(`SensorSource`) → AW5-01 (`warden_actions!`). Die vier bauen aufeinander auf;
ich sehe keine Möglichkeit, das zu parallelisieren.
**Es gilt als Reihenfolgeregel:** nie zwei Makro-Knoten gleichzeitig, und
jeder landet am **Anfang** seiner Welle — AW2-06 speist die elf Sensoren
derselben Welle, AW5-01 den `warden-proto`-Knoten derselben Welle.

**K19 · `deny_unknown_fields` steht auf 77 Structs in neun Crates, nicht 17 —
und ist keine workspaceweite Konvention.**
Verteilung: `harw-config` 38, `harw-protocol` 15, `harw-channel-browser` 11,
`harw-agent-dsl` 7, Rest einzeln. **41 Crates haben null.** Wichtiger als die
Zahl ist, was die Verteilung zeigt: es ist eine Konvention der **Wire- und
Konfigurations-Crates**.
**Es gilt:** „auf jedem Typ, der von außen kommenden JSON/TOML deserialisiert:
Wire-Envelopes, Konfiguration, Definitionen, Fixtures." Das ist prüfbar und
deckt AW5-02 (`WardenRequest`) und AW6-02 (Verdikt-Vertrag) ab, die beide
genau dieser Klasse angehören. Als Pauschalregel für ~50 neue Crates wäre sie
entweder überzogen oder würde ignoriert.

**K20 · Die `can_spawn`-Anekdote ist überholt.**
Der Leitfaden erzählt sie als „dokumentiert als erzwungen und nie
aufgerufen". Seit der Baseline ruft `harw-core/src/child_controller.rs:1410`
`harw_agent_dsl::roles::can_spawn(...)` und lehnt bei `false` fail-closed ab;
zwei Integrationstests belegen die Durchsetzung in `admit()`.
**Es gilt die heute wahre Fassung derselben Lehre:** „Die Lücke wurde durch
eine Bottom-up-Analyse gefunden, nicht durch Tests, weil die Tests die
Dokumentation prüften. Die Lehre ist nicht, dass Prüfungen vergessen werden —
die Lehre ist, dass eine vergessene Prüfung **lange unentdeckt** bleibt,
während ein fehlender Typ am ersten Tag nicht kompiliert."
*Direkte Programmfolge:* AW1-04s Kriterium „`can_spawn`-Ablehnungen
emittieren" ist jetzt umsetzbar. Mit der alten Anekdote im Kopf hätte der
Knoten erst die Prüfung gebaut — oder eine zweite daneben.

**K21 · `partition_write_sets` und `members_from_plan` sind falsch verortet —
mit Kantenfolge.**
`partition_write_sets` liegt in **`harw-plan::graph`** (Aufruf:
`harw-ops/src/plan.rs:1159`), nicht in `harw-plan-bridge::cells`.
`members_from_plan` ist ein **Feld** auf `RawCellSpec`/`CellSpec`
(`harw-agent-dsl/src/organization.rs:242`, `pub members_from_plan: String`),
ein Plan-Pfad-Glob, aufgelöst über `ScopeMatcher::matches_glob`.
**Es gilt:** „Eine Zelle deklariert ihr Revier als Glob im Feld
`members_from_plan`; `ScopeMatcher::matches_glob` löst es gegen die
Plan-Knoten auf; `harw_plan::graph::partition_write_sets` zerlegt das
Ergebnis in Batches mit paarweise disjunkten Schreibmengen;
`harw_core_bridge::fanout_children` fährt sie; `tighten_budget` verschärft
die Kind-Budgets."
Das ist keine reine Ortskorrektur: **`harw-plan-bridge` besitzt die
Partitionierung nicht, `harw-plan` tut es.** Ein Knoten, der die
Zellenmechanik erweitert (AW6-03: „Fan-out über Zellen mit `write_partition`
None"), schreibt damit in `harw-plan/**` — und `harw-plan` wird bereits von
AW4-04 geschrieben. Das wäre eine weitere unentdeckte Kollision gewesen.

**K22 · Die Zahlen im Index sind nicht gegen den Arbeitsplan nachgezählt.**
„79 Arbeitsknoten" lässt sich nicht rekonstruieren (72 aufgezählt), „24 plus
Fassade" DoD-Crates stimmt nicht mit den eigenen Kapiteln überein (27 Libs +
Fassade + 4 Binaries), Anhang A ist an 16 Stellen unvollständig.
**Es gilt:** alle drei Zahlen werden von AW0-10 **generiert**, nicht gepflegt.

---

## Nicht existierende Voraussetzungen

Mehrfach zitiert, teils als vorhanden. Keiner existiert — alle sind
Arbeitsknoten, kein Fundament:

`harw-context`, `ContextCeiling`, `TrustClass`, `DetailMode`, `ReadScope`,
`ContentDigest`, `Action<Authorized>`, `FreezeStore`, `harw-observe`,
`TelemetrySink`, `MetricKey`, `TraceContext`, `FieldName`, die fünf Makros
(`metrics!`, `#[traced]`, `#[derive(SensorSource)]`, `warden_actions!`,
`#[derive(Redact)]`), `xtask`, `ci/`, `deploy/`, `webui/`, `deny.toml`,
`cargo-vet`.

**Eine Ausnahme, die auf der Liste stand und nicht dorthin gehört:**
`ContextProgram` **existiert** — `harw-agent-dsl/src/executable.rs:221`, mit
`must_include`/`exclude`/`context_policy`, und ist bereits Teil der
`SnapshotId` (Zeilen 461-466). Was fehlt, ist die **Durchsetzung**:
`harw-core/src/context_budget.rs:49` nimmt Fragmente gierig in
**Ankunftsreihenfolge** auf, gespeist aus `Vec<Arc<dyn ContextProvider>>` in
Registrierungsreihenfolge (`harw-extension-api/src/registry.rs:62`). Das ist
exakt die B1-Regression, die AW1-03 behebt — und der Beweis, dass nur die
Durchsetzung fehlt, nicht der Typ.

---

## Was der Ist-Code bereits trägt

Geprüft und bestätigt, in der Form, die die Dokumente annehmen:

`SandboxSpec` / `PermissionSet` / `NetworkScope` (nur `intersection`, kein
`union`) · `ChildLeaseStore` mit atomarem Claim · `LeaseToken{epoch,nonce}` ·
`ApprovalActor` / `ApprovalStore` · `ChildReturnContract` (dritter Arm,
vierter additiv möglich) · `ExecutableAgentIr` mit `ContextProgram` und
SnapshotId-Hashing · `AgentRoleId` mit `can_spawn` (fail-closed aufgerufen) ·
Familien, Clans, Zellen · `harw-code-graph` (`WorkspaceGraph`,
`topological_levels`, `parse_lockfile`) · `harw-research` (Frage, Finding,
`ReturnEnvelope`) · `fanout_children`, `tighten_budget` · `PlanController`
mit genau neun `ReconcileStep`-Varianten · `MutationContract`,
`validate_patch` · `harw_plan::graph::partition_write_sets` · `EvidenceRef`
(ohne Digest — additiv erweiterbar) · `GoalContextProvider` mit „##
Invarianten"-Abschnitt · `harw-secrets::audit::chain::verify` ·
`harw-knowledge` mit echtem BM25 (K1=1.2, B=0.75, Caps 256/8),
`superseded_by`, vier `ContradictionReason`-Varianten ·
`VisibilityScope::OperatorOnly` · `harw-mcp-server` auf hyper (nicht axum) ·
`harw-operations` mit vier `PermissionTier` und drei `Surface`-Varianten ·
`harw_macros::{HarwId, KebabEnum}` mit Konsumenten.

---

## Die korrigierte Wellenstruktur

**93 Knoten statt 72.** Der Zuwachs: 6 fehlende Crates (K13), 7 Teilungen zur
Kollisionsauflösung, 3 Strukturknoten (AW0-00, AW0-11, AW6-00).

**Legende:** **[T]** geteilt (Parallelität bleibt), **[S]** sequenziert
(Kante eingezogen), **[V]** verlagert (Schreibbereich woandershin),
**⭐** neuer Knoten.

### AW0 — Vokabular und Fundament (12 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| **AW0-00** ⭐ | Workspace-Fundament: `[workspace.dependencies]` (ein `jiff`), `[workspace.lints] unsafe_code = "forbid"`, ~50 Stub-Crates, `xtask/src/main.rs`, drei `unsafe`-Testhelfer saniert | `/Cargo.toml`, alle neuen Crate-Stubs, `xtask/src/main.rs`, `harw-secrets/src/kek.rs`, `harw-browser/src/{host,session}.rs` | — |
| AW0-01 | `harw-observe` **plus File-Sink** (K2): `FieldName`, `MetricKey`, `TelemetrySink`, `NullSink`, `TraceContext`, `ObserveError`, JSONL-Rotation mit Prüfsumme | `harw-observe/**`, `harw-observe-file/**` | AW0-00, AW0-11 |
| AW0-03 | ID-Newtypes über `#[derive(HarwId)]`, `ContentDigest` (blake3), **`Confidence` nach `harw-types` gezogen** (K3) | `harw-types/**`, `harw-memory/src/epistemic.rs`, `harw-model-catalog/src/provenance.rs` | AW0-00 |
| **AW0-11** ⭐ | Pfadvokabular: Reports-Dir, Freeze-Dir, Ring-Snapshots | `harw-home/**` | AW0-00 |
| AW0-10 | CI-Gates (verbotene Kanten über `harw-code-graph`) + **Anhang-A-Generator** (K9, K22) | `xtask/src/gates.rs`, `Makefile` | AW0-00 |
| AW0-02 | `metrics!`, `field!`, `#[derive(Redact)]` + trybuild-Korpus | `harw-macros/**` | AW0-01 |
| AW0-06 | `Capability`, `ReadScope`, `SensorHandle<Unbound\|Bound>`, **`trait Sensor`**, `SensorError` | `harw-dod-cap/**` | AW0-03 |
| AW0-08 | Retrieval-Vokabular **+ Rangtypen** (K11): `Chunk`, `SourceRef`, `IndexManifest`, `Ranked`, `BudgetSpec`, `CostEstimator`, `Packed` | `harw-lens-types/**` | AW0-03 |
| AW0-07 | Sicherheitsvokabular: `HostSample`, `SecurityEvent`, `Actor{auid}`, `Finding<S>` **mit korrigierter Konstruktionsregel** (K12), `SecurityEvidence` | `harw-dod-signals/**` | AW0-01, AW0-06 |
| AW0-09 | Reine Rangfunktionen `rrf_fuse`, `mmr`, `collapse`, `pack` (ohne I/O, ohne Systemzeit) | `harw-lens-rank/**` | AW0-08 |
| AW0-04 | Kontextvokabular: `Selector`, `Fragment` v2, `TrustClass`, **`FragmentOrigin`** (K15), `ContextBudgetSpec::tighten`, `ContextCeiling::{intersect,admits}` | `harw-context/**` | AW0-03, **AW0-08** |
| AW0-05 | `Fragment::from_v1` als einziges Konvertierungssymbol; 13 Konsumenten unverändert | `harw-extension-api/**` | AW0-04 |

### AW1 — Selbstbeobachtung und Assembly v2 (7 Knoten)

**Die wichtigste Welle.** Hier wird das `ContextProgram` erstmals durchgesetzt.

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| AW1-01 | `TraceContext` persistieren, serde-additiv, Bestandsdateien lesbar | `harw-job-runtime/**`, `harw-session-store/**` | AW0-01 |
| AW1-02 | `#[traced]`: `Instrument` statt `enter`, Felder lazy, Argumente über `Redact` | `harw-macros/**` | AW0-02 |
| AW1-05 | Plan-Schleifen-Metriken (Reconcile-Schritte, Invalidations, Goal-Coverage, Scope-Violation-Rate) | `harw-plan-bridge/**` | AW0-02 |
| AW1-07 **[T]** | Nullzähler-**Mechanik** (K7); konkrete Zähler wandern zu ihren Invarianten | `harw-observe/**` | AW0-02 |
| AW1-03 **[T]** | `Assembly<Gathered\|Admitted\|Budgeted\|Rendered>` **+ eigene Metriken**; deterministisch nach Sektion/Stärke/Stability, **nicht** nach Ankunft; Property-Test gegen Permutation der Provider-Reihenfolge | `harw-core/src/{context_budget,turn_loop}.rs` | AW0-02, AW0-04, AW0-05 |
| AW1-04 **[T][S]** | Kind- und Lease-Metriken, `can_spawn`-Ablehnungen | `harw-core/src/{child_controller,session}.rs` | AW1-02, **AW1-03** |
| **AW1-04b** ⭐ **[T]** | Selektor- und Executor-Metriken | `harw-memory/src/context_selector.rs`, `harw-tools/src/executor.rs` | AW1-02 |

### AW2 — Programme und Beobachter (20 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| AW2-01 | `[context]`-Grammatik, `harwness.context.<name>@<v>` durch die vierstufige Kaskade, Resolver ruft `ceiling.admits` | `harw-agent-dsl/**` | AW0-04, AW1-03 |
| AW2-03 | `harw-dod-readfs`: getypte Lesezugriffe, Symlink aus dem Scope → `OutsideScope` | `harw-dod-readfs/**` | AW0-06 |
| **AW2-04** ⭐ | `harw-dod-netlink` (K13) | `harw-dod-netlink/**` | AW0-06 |
| AW2-06 **[S]** | `#[derive(SensorSource)]`: `poll`, Metrikemission, Capability-Deklaration | `harw-macros/**` | AW0-06, **AW1-02** |
| AW2-05 | `sensor_suite!`-Harness, Verzeichniskonvention (`tree`, `expect.json`, `malformed`, `adversarial`), sechs geerbte Prüfungen | `harw-dod-fixtures/**` | AW0-06, AW2-03 |
| AW2-02 **[S]** | `ContextCeiling` im `SpawnInput`, geschnitten im selben Schritt wie die Permissions; Attacker-Fixture wird abgewiesen | `harw-core/src/child_controller.rs`, `harw-extension-api/**` | AW2-01, **AW1-04**, AW0-05 |
| AW2-20 **[S]** | `program_defaults_for`; Programm darf nur verengen | `harw-model-catalog/**` | AW2-01 |
| **AW2-07..13** | **Sieben** Strom-A-Sensoren (K6): thermal, cpu, memory, blockio, netcounters, gpu, cgroup | `harw-dod-{thermal,cpu,memory,blockio,netcounters,gpu,cgroup}/**` | AW2-03, AW2-05, AW2-06 |
| AW2-14 | `harw-dod-listener`: offene Listener mit cgroup-Bezug, keine Verbindungsinhalte | `harw-dod-listener/**` | AW2-03, AW2-05 |
| AW2-15 | `harw-dod-authlog`, Journal-Backend zuerst, `auid` extrahiert | `harw-dod-authlog/**` | AW2-03, **AW2-04**, AW2-05 |
| AW2-16 | `harw-dod-scanreport`: liest nur Reports, kein Codepfad bildet eine Kommandozeile | `harw-dod-scanreport/**` | AW2-03, AW2-05 |
| AW2-17 | `harw-dod-workspace` über `harw-code-graph`; `StructureDrift`-Ereignisse | `harw-dod-workspace/**` | AW2-05 |
| AW2-18 | Sentinel-**Bibliothek**: Degradationsautomat, Ringpuffer mit `freeze()` | `harw-dod-sentinel/**` | AW2-07..AW2-17 |
| **AW2-19** ⭐ **[T]** | Sentinel-**Binary**: IPC-Empfangspfad (`SOCK_SEQPACKET`), Landlock-Bindung | `harw-sentinel/**` | AW2-18 |

Bibliothek/Binary getrennt **[T]**, weil die Lib ohne Privilegien testbar ist
und das Binary den Empfangspfad trägt — zwei Testregime, zwei Reviewer.

### AW3 — Mengen und Quellen (6 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| AW3-01 | **`NetworkScope` um `EgressTarget::Cidr` erweitern** (K1), additiv | `harw-sandbox/**` | AW0-03 |
| AW3-03 **[S]** | Quellenbindungen: `PlanContextProvider`, `selection_role_for`, `#[context_provider]` um Namensraum/Trust/Cost erweitert | `harw-plan-bridge/**`, `harw-memory/**`, `harw-knowledge/**` | AW1-03, **AW1-04b**, **AW1-05**, AW2-01 |
| AW3-04 **[S]** | `harw-observe-prom`: Textformat aus `MetricKey`, Loopback/Unix-Socket, Golden-Test | `harw-observe-prom/**` | AW0-01, **AW1-07** |
| AW3-05 | `harw-lens-chunk`: drei Strategien, deterministische Digests, Relationsvorschläge | `harw-lens-chunk/**` | AW0-08 |
| AW3-06 | `harw-lens-store` nach `harw-session-store`-Muster (fs4-Lock, `sync_data`, `persist`) | `harw-lens-store/**` | AW0-08, AW0-11 |
| AW3-02 | `harw-dod-netpolicy` als reiner Plan, `rustables` hinter eigenem Trait | `harw-dod-netpolicy/**` | AW3-01, AW0-06 |

### AW4 — Grenze und Regeln (9 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| AW4-01 **[S]** | Zwei-Block-Rendering, Vertrauensklassen, Pinned-Fragmente digest-stabil, Nullzähler `trust_block_violation` | `harw-core/**`, `harw-instructions/**` | AW1-03, **AW2-02**, AW3-03 |
| AW4-02a **[T]** | `harw-dod-fsmon` (Lib): fanotify auf `rustix`, loginuid über procfs | `harw-dod-fsmon/**` | AW0-06, AW2-05 |
| AW4-04 | `EvidenceRef.digest: Option<ContentDigest>`, `serde(default)`, fünf Konsumenten unverändert | `harw-plan/src/types.rs` | AW0-03 |
| AW4-05 **[S]** | `ArtifactKind`-Erweiterung, `OperatorOnly` beim Recall, **`Confidence` → `PalaceStatus`** (K3) | `harw-knowledge/**` | AW0-07, **AW3-03** |
| AW4-06 | `harw-lens-index`: `VectorIndex`-Trait, `FlatIndex`, `Bm25Index`, Manifest-Prüfung | `harw-lens-index/**` | AW0-08, AW3-06 |
| AW4-07 | `harw-lens-embed`: vier Schichten, `EmbeddingRole`-Router, `Confidential` filtert auf `Locality::Local` | `harw-lens-embed/**` | AW0-08 |
| AW4-02b **[T]** | `harw-probe-fs` (Binary): Push-Only, `CAP_SYS_ADMIN` allein | `harw-probe-fs/**` | AW4-02a |
| AW4-03 | `harw-dod-rules`: `Rule`-Trait, injiziertes `now`, **`Finding<Raw>`/`<RuleChecked>` entstehen hier** (K12), `epistemic_confidence_for` (K3) | `harw-dod-rules/**` | AW0-07, AW3-01, AW4-02a |
| **AW4-08** ⭐ | CIDR-Aufrufstellen migrieren (K1) | `harw-core/src/session.rs`, `harw-tools/src/sandbox_guard.rs`, `harw-tool-web/src/fetch.rs` | AW3-01, **AW4-01** |

### AW5 — Durchsetzung und Referenzen (11 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| AW5-01 **[S]** | `warden_actions!`: Wire-Enum, `ProposedAction`, Tool-Schema, Audit-Typ, Zulässigkeitsmatrix | `harw-macros/**` | AW0-07, **AW2-06** |
| **AW5-05** **[S]** ⭐ | `FreezeStore`, Schlüssel **`(CgroupId, FindingId, frozen_at)`** (K5); Reconciliation beim Start vor jeder anderen Operation | `harw-session-store/**` | AW0-11, **AW1-01** |
| AW5-06 **[S]** | Namensraum-Routing: `security.*` und `warden.*` nur File-Sink, Export nur nach Operator-Bestätigung | `harw-observe/**` | **AW1-07**, AW3-04 |
| AW5-07 **[S]** | `DetailMode::References`, `context.load` als Tool mit Kappe, Historie wird `history.tail` | `harw-context/**`, `harw-core/**`, `harw-tools/**` | **AW4-01**, **AW4-08**, **AW1-04b** |
| AW5-08 | Erste zwei Indizes (`docs.design`, `knowledge.palace`), Sichtbarkeit als getrennte physische Indizes | `harw-lens-source/**`, `harw-lens-query/**` | AW0-09, AW3-05, AW4-06, AW4-07 |
| AW5-09 **[S]** | `ContextProposal` als Typ und Artefakt, Review-Fläche, deterministische Heuristik | `harw-knowledge/**`, `harw-ops/src/context_proposal.rs`, `harw-ops/src/lib.rs`, `harw-ops/Cargo.toml` | **AW4-05** |
| AW5-02 | `harw-dod-warden-proto`: `deny_unknown_fields`, keine freien Argumente, `AuthorizationProof` | `harw-dod-warden-proto/**` | AW5-01 |
| **AW5-10** ⭐ | `harw-lens`-Fassade (K13) | `harw-lens/**` | AW5-08 |
| AW5-04a **[T]** | Warden (Lib): Proof-Nachprüfung, Audit vor jedem Fehlerpfad | `harw-dod-warden/**` | AW3-02, AW5-02 |
| **AW5-03** | Leiter und Freeze: `Action<Proposed\|Authorized>`, `authorize` `pub(crate)`, `Ladder::admissible`, `Finding<Triaged>` | `harw-dod-escalate/**` | AW4-03, AW5-02, **AW5-05** |
| AW5-04b **[T]** | Warden (Binary): systemd-Socket mit `SO_PEERCRED` | `harw-warden/**` | AW5-04a |

### AW6 — Agenten (11 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| **AW6-00** ⭐ **[V]** | Registry-Manifest: Verzeichniskonvention statt `include_str!`-Liste (K8) | `harw-registry-defaults/src/**` | AW2-01 |
| AW6-04 **[S]** | Plan-Andockung: `evidence_for_security_finding` über `offset_from_timestamp`, `ContractViolation` → Invalidate-**Vorschlag** | `harw-plan-bridge/**`, `harw-dod-escalate/**` | AW4-04, AW5-03, **AW3-03** |
| AW6-06 **[S]** | Layer-4-Rückkanal: durabler Dream-Job, schlägt vor, committet nie | `harw-model-catalog/**`, `harw-knowledge/**` | AW1-05, **AW2-20**, **AW5-09** |
| AW6-07 **[S]** | Lens-Föderation: `IndexSelector`, RRF über Indizes, **Assembly ruft dieselbe `pack`-Funktion** | `harw-lens-federation/**`, `harw-lens-query/**`, `harw-context/**` | AW5-08, **AW5-10**, **AW5-07** |
| **AW6-09** ⭐ | `harw-dod`-Fassade (K13) | `harw-dod/**` | AW2-18, AW5-03, AW5-04a |
| AW6-01 **[V]** | Security-Familie und Clan, Disjunktheitstest der `universe`-Mengen | `harw-agent-dsl/src/**`, `harw-registry-defaults/agents/families/security/**` | AW2-01, AW2-02, AW6-00 |
| AW6-05 **[V]** | Programmbibliothek (9 Programme, alle per `extends` von `base`), Golden Render je Programm | `harw-registry-defaults/agents/context-programs/**` | AW6-00, AW5-07 |
| **AW6-10** ⭐ | `harw-tool-lens` (K13) — ohne diesen Knoten erreicht Lens nie einen Agenten | `harw-tool-lens/**` | AW5-10, AW6-07 |
| AW6-02 **[S]** | Verdict-Vertrag `harwness.security-verdict/v1`, `ChildReturnContract`-Arm | `harw-dod-signals/**`, `harw-core-bridge/**` | AW6-01, **AW4-04**, **AW0-07** |
| AW6-08 **[V][S]** | Context Steward + drei Nullzähler | `harw-registry-defaults/agents/roles/context-steward/**`, `harw-knowledge/**` | AW6-05, **AW6-06** |
| AW6-03 **[V]** | Vier Triage-Spezialisierungen, Fan-out über Zellen mit `write_partition` None | `harw-registry-defaults/agents/roles/security-*/**`, `harw-plan/src/graph.rs` (K21) | AW6-02, AW6-00 |

### AW7 — Betrieb und Härtung (9 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| AW7-01a **[T]** | eBPF-Ladeschicht hinter Trait, keine `aya`-Typen in öffentlichen Signaturen | `harw-dod-bpf/**` | AW0-06 |
| AW7-02 | `harw-observe-otlp`: HTTP statt gRPC, Metrikbrücke zuerst | `harw-observe-otlp/**` | AW5-06 |
| AW7-04 | Audit-Spiegel off-host, Out-of-Band-Meldeweg, `audit_chain_break` | `harw-secrets/**`, `harw-channel-*/**` | AW5-04b |
| AW7-05 **[S]** | Restliche Indizes, vierte Embedding-Schicht, Nullzähler `lens_remote_embed_on_operator_only` | `harw-lens-source/**`, `harw-lens-embed/**` | AW6-07, **AW5-08**, **AW4-07** |
| AW7-06 **[V][S]** | Intel-Scout, Advisory-Join gegen `Cargo.lock` über `find_locked` | `harw-dod-rules/**`, `harw-registry-defaults/agents/roles/intel-scout/**` | AW6-01, AW2-17, **AW4-03**, **AW6-00** |
| AW7-01b **[T]** | `harw-dod-procmon` | `harw-dod-procmon/**` | AW7-01a, AW2-05 |
| AW7-01c **[T]** | `harw-dod-flow` | `harw-dod-flow/**` | AW7-01a, AW2-05 |
| AW7-01d **[T]** | `harw-probe-bpf` (Binary) | `harw-probe-bpf/**` | AW7-01b, AW7-01c |
| AW7-03 **[S]** | systemd-Units, vier Privilegienklassen, Landlock-Scope je Unit | `harw-install/**`, `deploy/**` | AW5-04b, AW7-01d, **AW2-19**, **AW4-02b** |

AW7-01 in vier geteilt **[T]**: `procmon` und `flow` sind nach C7
Geschwister, die einander nicht kennen dürfen — sie in einem Knoten zu bauen
ist genau die Kopplung, die C7 verbietet. AW7-03 hängt an **allen vier**
Binaries; ohne sie lässt sich das Privilegienbudget-Gate nicht auswerten.

### UI — Control Plane, quer zu AW0–AW7 (8 Knoten)

| ID | Titel | Schreibbereich | Hängt an |
|---|---|---|---|
| UI-00 | `harw-web`, `Surface::Web`, `SO_PEERCRED`, SSE mit Sequenznummer, kein zweiter Autoritätspfad | `harw-web/**`, `harw-operations/**` | AW0-00 |
| UI-01 **[T]** | Next.js-**Schale** + Typgenerator; Datenblock-Renderer ohne Markdown, ohne aktive Links | `webui/{package.json,tsconfig.json,next.config.*,app/layout.tsx,app/globals.css,lib/**,components/ui/**}`, `xtask/src/webui.rs` | UI-00, **AW0-10** |
| UI-02 | Master-Chat und Session-Chat, Daumen-Runter → Memory-Signal | `webui/app/(chat)/**` | UI-01 |
| UI-03 | Sessions, Agentenbaum, Plan und Ziel, Vorschlagswarteschlange | `webui/app/(sessions)/**`, `webui/app/(plan)/**` | UI-01 |
| UI-04 | Telemetrie und Nullzähler-Leiste | `webui/app/(telemetry)/**` | UI-01, AW1-07 |
| UI-05 | Sicherheitszentrale, lesend | `webui/app/(security)/**` | UI-01, AW5-03, **AW4-01** |
| UI-07 | Verwaltung: Modelle vierschichtig, Definitionen mit Ceiling und SnapshotId | `webui/app/(admin)/**` | UI-01, AW6-05 |
| UI-06 | Bestätigungsfläche: irreversible Aktionen als `ApprovalRequest`, nie als Knopf | `webui/app/(security)/**`, `harw-web/src/security.rs` | UI-05, AW5-04b |

`webui/**` von UI-01 auf die Schale eingeengt **[T]**; die künstliche Kette
UI-02→03→04→05→07 aufgelöst — die Route-Gruppen sind disjunkt, die Kanten
waren Lesereihenfolge, nicht Abhängigkeit. UI-05 hängt neu an AW4-01: sonst
rendert die UI angreiferkontrollierte Sensorfelder, bevor die
Trust-Klassifikation existiert. Das war ein Loch in U8.

---

## Parallelisierung

Echte Crate-Disjunktheit nach Auflösung aller Kollisionen, in
Ausführungsebenen:

| Welle | Ebene 1 | Ebene 2 | Ebene 3 | Ebene 4 | Ebene 5 | max |
|---|---|---|---|---|---|---|
| **AW0** | 1 (00) | **4** (01, 03, 11, 10) | **3** (02, 06, 08) | **3** (07, 09, 04) | 1 (05) | **4** |
| **AW1** | **4** (01, 02, 05, 07) | 1 (03) | **2** (04, 04b) | | | **4** |
| **AW2** | **4** (01, 03, 04, 06) | **3** (05, 02, 20) | **2 seriell + 9 parallel** | 1 (18) | 1 (19) | **9** |
| **AW3** | **5** (01, 03, 04, 05, 06) | 1 (02) | | | | **5** |
| **AW4** | **6** (01, 02a, 04, 05, 06, 07) | **3** (02b, 03, 08) | | | | **6** |
| **AW5** | **6** (01, 05, 06, 07, 08, 09) | **2** (02, 10) | **2** (04a, 03) | 1 (04b) | | **6** |
| **AW6** | **5** (00, 04, 06, 07, 09) | **3** (01, 05, 10) | **2** (02, 08) | 1 (03) | | **5** |
| **AW7** | **5** (01a, 02, 04, 05, 06) | **2** (01b, 01c) | 1 (01d) | 1 (03) | | **5** |
| **UI** | 1 (00) | 1 (01) | **5** (02, 03, 04, 05, 07) | 1 (06) | | **5** |

**Kritischer Pfad: 29 sequenzielle Ebenen für 85 Wellenknoten.** Die
UI-Kette (4 Ebenen) läuft quer und ist nie der Engpass.

### Die Elfergruppe in AW2 wird zu 2 + 9

Sie liegt fast genau auf dem Präzedenzfall des Vorprogramms (13 Agenten,
17.000 Zeilen, 26 Fehler, **alle** Vertragsdrift) und ist zugleich die am
besten geformte Fan-out-Gelegenheit hier: alle elf implementieren denselben
Trait, werden vom selben Makro erzeugt, erben dieselben sechs Prüfungen aus
demselben Harness — und **C7 verbietet ihnen ausdrücklich, einander zu
kennen**, die Schreibbereiche sind also nicht zufällig disjunkt, sondern per
Invariante. Die Vertragsfläche ist minimal: ein Trait, zwei Ausgabetypen, ein
Fehlertyp, ein Konstruktorparameter.

Genau deshalb ist es die gefährlichste Stelle: **jede Drift in `Sensor`,
`SensorError`, `ReadScope` oder `sensor_suite!` schlägt elffach gleichzeitig
auf.** Zwei Konsequenzen:

1. AW2-03, AW2-05 und AW2-06 (readfs, Harness, Makro) sind
   **Vertragsknoten, keine Arbeitsknoten**. Sie landen vollständig und
   kompilieren, bevor irgendein Sensor startet.
2. Der Leitfaden sagt „`sensor_suite!` steht vor dem dritten Sensor" —
   **zu spät**. Korrigiert: Harness und Makro stehen vor dem **ersten**, und
   die ersten **zwei** Sensoren (`thermal` als Sysfs-Fall, `cpu` als
   Procfs-Fall) werden **seriell** gebaut, um Trait und Harness gegen zwei
   verschiedene Quellformen zu validieren. Erst dann fahren die restlichen
   **neun** parallel.

Das kostet zwei serielle Knoten und schützt neun parallele.

### Obergrenze für heterogene Gruppen: sechs

Die Sechsergruppen in AW4 und AW5 sind crate-disjunkt, aber **vertraglich
heterogen** — sechs Agenten bauen sechs verschiedene Dinge gegen sechs
verschiedene Vokabulare. Die Driftwahrscheinlichkeit pro Agent ist dort höher
als bei den Sensoren, obwohl die Gruppe kleiner ist.
**Regel: Gruppen über sechs nur, wenn die Knoten strukturgleich sind.**

---

## Der Contract-Master

Die Lehre aus dem Vorprogramm ist präzise: **Verträge, die beim Schreiben
schon kompilierten, erzeugten null Fehler; gleichzeitig erfundene 26.**
Daraus folgt seine Form:

> **Der Contract-Master ist kein Dokument. Er ist die `pub`-Fläche der
> AW0-Knoten, als kompilierender Code, gelandet und getaggt, bevor die erste
> mindestens drei Knoten breite Gruppe startet.**

Ein Markdown-Dokument, das Signaturen paraphrasiert, reproduziert die 26
Fehler — es ist genau die Sorte Artefakt, aus der ein Agent eine Signatur
*ableitet* statt sie zu *lesen*. Das Dokument daneben ist nur der Index:
welches Symbol wo, wer besitzt es, wer darf es konstruieren.

**Vor AW0 Ebene 2** muss AW0-00 stehen. **Vor AW2 Ebene 3** (dem Fan-out)
müssen A bis H vollständig kompilieren.

**A · `harw-observe` (AW0-01), ~41 Konsumenten.** `FieldName` (nur über
`field!` konstruierbar) · `MetricKey { name, kind, unit, labels, cardinality }`
inklusive `MetricKind`, `Unit`, `Cardinality` als geschlossene Enums ·
`MetricValue` · **`trait TelemetrySink`** mit den drei Driftpunkten
verbindlich entschieden: Receiver, Rückgabetyp, Sync/Async · `TraceContext`
**mit Serde-Repräsentation** · `ObserveError` · `trait Redact` und die
Ausgabeform von `#[derive(Redact)]` (Default `Omitted`) · `NullSink`.

**B · `harw-types` (AW0-03).** `FindingId, SensorId, ActionId, BaselineId,
HostId, CgroupId` — **über das vorhandene `#[derive(HarwId)]`, nicht
handgeschrieben**; sechs handgerollte Newtypes wären genau die
Vervielfachung, die der Leitfaden verbietet · `ContentDigest` mit `Display`,
`FromStr` und Serde-Form · `Confidence` (aus K3) · **vor AW0-08 zu
entscheiden: ist `ChunkDigest` gleich `ContentDigest`, ein Newtype darüber
oder ein eigener Typ?** Drei Knoten und die Kollaps-Fixtures hängen daran.

**C · `harw-lens-types` (AW0-08).** `Chunk`, `SourceRef`, `ByteSpan`,
`IndexManifest`, `Metric`, `Locality` · **neu hierher verlagert (K11):**
`Ranked`, `BudgetSpec`, `trait CostEstimator`, `EdgeIndex`, `CollapsePolicy`,
`Packed`.

**D · `harw-lens-rank` (AW0-09).** Die vier Signaturen wörtlich. `pack` ist
**der einzige echte Cross-Subsystem-Vertrag zwischen Lens und Kontext** —
AW6-07s Abnahme macht ihn zum Testgegenstand. Er gehört nach AW0, nicht AW6.

**E · `harw-context` (AW0-04).** `Selector`, `FragmentLabel` · `Fragment` v2
mit allen sieben Feldern, `TrustClass`, `Stability`, `CostEstimate` ·
**`FragmentOrigin`** statt `Provenance` (K15) · `ContextBudgetSpec` +
`tighten` · `ContextCeiling` + `intersect` + `admits`, `CeilingViolation` ·
`ResolvedContextProgram`, `Section`, `SectionName`, `SelectionRole`,
`DetailMode`, `OmissionReason` · **`Fragment::from_v1`** — der einzige
Übergang für 13 bestehende Konsumenten; ändert sich die Signatur nach AW0-05,
brechen 13 Crates.

**F · `harw-dod-cap` (AW0-06) — höchste Konsequenz.** Elf Crates
implementieren das in einem Batch: `Capability` mit `class()` und `probe()` ·
`ReadScope` mit `intersection` und `open()` (Symlinks **vor** der Prüfung
aufgelöst) · `SensorHandle<Unbound|Bound>` · `SensorError` mit
`permanence()` · **`trait Sensor`**. Dazu die Ausgabeform von
`#[derive(SensorSource)]` und die Verzeichniskonvention von `sensor_suite!` —
beides ist Vertrag, obwohl es formal in AW2 liegt.

**G · `harw-dod-signals` (AW0-07).** `HostSample`, `SecurityEvent`,
`EventKind`, `Actor{auid}`, `Hardness`, `Severity`, `SecurityEvidence` mit
Digest · **`Finding<S>` mit der Konstruktionsregel aus K12.** Wird die nicht
*vor* AW2 entschieden, scheitern elf Agenten gleichzeitig an einem
`pub(crate)`, das sie nicht sehen.

**H · Workspace-Vertrag (AW0-00).** Die eine `jiff`-Version ·
`[workspace.lints.rust] unsafe_code = "forbid"` · die
`#[derive(HarwError)]`-Konvention für ~50 neue Fehlertypen (ein Typ pro
Crate, `source()`, an Grenzen verdichtet) · die `[lints] workspace = true`-
Zeile, die jeder Stub schon trägt.

### Die Regel, die das Ganze trägt

**Kein Agent einer Parallelgruppe definiert je einen Typ, den ein anderer
Agent derselben Gruppe importiert.** Wenn zwei Knoten derselben Ebene einen
Typ teilen, gehört er nach unten in einen früheren Knoten — auch wenn das
eine Welle mehr kostet. Im korrigierten Graphen ist das durchgehend erfüllt;
der einzige Verstoß war der `pack`-Widerspruch (K11), und das ist genau die
Sorte Fehler, die im Vorprogramm 26 Compile-Fehler erzeugt hat.

---

## Reihenfolge-Zwänge

**„AW1 vor allem anderen" ist zu schwach formuliert.** AW1-03 setzt das
`ContextProgram` für **eine** Session durch. Hereditär wird die Zusage erst
durch AW2-02 (`ContextCeiling` im `SpawnInput`). Dazwischen gilt: ein Kind
kann ein Programm tragen, dessen Decke niemand geschnitten hat.
**Korrigiert: AW1-03 → AW2-01 → AW2-02 ist eine geschlossene Kette; zwischen
AW1-03 und AW2-02 landet kein Knoten, der Kinder erzeugt oder Kindkontext
beeinflusst.**

**„AW4 vor AW6" stimmt, aber die Begründung verlangt mehr als die Kante.**
Der Triage-Agent liest angreiferkontrollierte Sensorfelder. Er braucht nicht
nur AW4-01, sondern auch AW0-07s `SecurityEvidence`-Digest und AW4-04s
`EvidenceRef.digest`, damit `TrustClass::Evidence` überhaupt vergeben werden
kann. **Ohne Digest ist jedes Sensorfeld `Data`, und die dritte
Vertrauensklasse ist leer.** → AW6-02 hängt zusätzlich an AW4-04.

**`harw-observe` ist die tiefste Wurzel — tiefer als `harw-types`,** weil
`harw-types` nur IDs liefert, während `harw-observe` einen Trait liefert, den
jede Crate implementiert oder aufruft. **Rund 41 von 50 neuen Crates hängen
transitiv daran.** Daraus:

1. **AW0-01 ist der einzige echte Programmblocker.** Nichts außer AW0-00,
   AW0-03 und AW0-11 darf davor.
2. **`TraceContext`s Serde-Form ist nach AW1-01 eingefroren** — sie landet in
   `StoredJob` und `ChildLeaseRecord`, beides Bestandsformate mit der
   Anforderung „bestehende Dateien bleiben lesbar". `TraceContext` gehört
   deshalb in den Contract-Master mit derselben Härte wie ein Wire-Typ.
3. **`MetricKey` ist nach AW3-04 eingefroren** — der Prometheus-Sink leitet
   Namen daraus ab und hat einen Golden-Test.

**Weitere harte Kanten:** AW0-00 vor allem · AW0-08 vor AW0-09 **und** vor
AW0-04 (K11) · `harw-macros` als vierwellige Serialkette, je am Wellenanfang
(K18) · AW5-05 vor AW5-03 (K5) · AW6-00 vor AW6-01/03/05/08 und vor AW7-06
(K8) · AW4-05 → AW5-09 → AW6-06 → AW6-08 (K8) · AW2-04 vor AW2-15 (K13) ·
alle vier Binaries vor AW7-03 · AW0-10 vor UI-01 (K14) · AW5-10 vor AW6-10 ·
AW4-01 vor UI-05.

---

## Verifikationsplan

**Bindende Arbeitsregel (Nutzervorgabe):** *Cargo läuft ausschließlich
sequenziell und ausschließlich bei mir — nie in einem Coding-Agenten.* Erst
reines Coding, dann Verifikation. Grund ist nicht Vorsicht, sondern Platz: N
parallele Agenten erzeugen N `target/`-Bäume, und der Host lief dabei schon
mit 0 MB frei voll. `/tmp` liegt auf derselben Partition; bei ENOSPC nennt die
Fehlermeldung `/tmp`, obwohl `target/` der Verursacher ist.

**Ablauf je Ausführungsebene:**

1. Coding-Agenten fahren die Ebene, jeder mit exklusivem Schreibbereich,
   **ohne cargo**.
2. Ich prüfe die WriteSet-Disjunktheit gegen die Knotentabelle, bevor ich
   verifiziere.
3. `CARGO_INCREMENTAL=0 cargo check --workspace` — die schnellste Stufe, die
   Vertragsdrift findet.
4. Erst am Wellenende: `cargo check --workspace --all-targets` (fängt, was in
   Tests driftet — im Vorprogramm fiel genau so ein Feldfehler erst hier auf).
5. `cargo clean` zwischen den Läufen; freien Platz vorher prüfen.

**Am Wellenende zusätzlich:**

- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace` **in drei Crate-Gruppen**, dazwischen
  `rm -rf target/debug/incremental` — die Platte trägt keinen Lauf am Stück.
- Tag setzen: `aw<N>-check-green`, `aw<N>-gates-green`.

**Programmspezifische Gates, sobald ihre Invariante existiert** (Anhang B,
alle über `xtask` + `harw-code-graph`): verbotene Kanten (ab AW0) ·
Privilegienbudget je Binary (ab AW2) · Warden-Abhängigkeitszahl, transitiv
gezählt inklusive Proc-Macros und Build-Deps (ab AW5) · kein C-Build im
Warden-Teilbaum (ab AW5) · keine Route ohne `OperationMeta` (ab UI-00) ·
Tier-Ablehnungsmatrix (ab UI-00) · Harness-Vollständigkeit je Sensor (ab AW2)
· OTel-Adapter-Isolation (ab AW7) · **Anhang-A-Generator: jeder
Schreibbereich, der von zwei Knoten ohne Pfad dazwischen berührt wird, ist
ein Gate-Fehler** (ab AW0).

**Abnahme des Gesamtziels** (aus dem Goal-Hook): Der Plan gilt erst als
erfüllt, wenn eine **Bottom-up-Codeanalyse mit Plan-Abgleich** bestätigt, dass
jeder Knoten gelandet ist — mit der vierten Spalte aus dem Vorprogramm:
**„Konsument existiert?"**, geprüft per Suche nach Aufrufen außerhalb der
eigenen Crate und außerhalb von Tests. Beide Gates grün beweisen nicht, dass
Code angeschlossen ist; im Vorprogramm war die gesamte Planungsfläche im Chat
unerreichbar, während clippy und `cargo test` grün waren. Dabei gilt: ein
ungenutztes Element ist nicht automatisch ein Versäumnis — die richtige Frage
ist nicht „warum wird es nicht benutzt", sondern „löst es das Problem, das
der Code tatsächlich hat".

---

## Risiken und offene Entscheidungen

### Jetzt entschieden

| # | Frage | Entscheidung |
|---|---|---|
| 1 | Warden-Dep-Obergrenze | **12 direkte `[dependencies]`-Einträge**, ohne Dev- und Build-Deps, jede einzeln begründet. Zweimal korrigiert (K53): transitiv zählen ist unerreichbar, nachgemessen **99** Laufzeit-Crates und 155 mit Proc-Macro-Zweigen. Stand: **12 von 12, ausgeschöpft**. Gate ab AW5. |
| 2 | `xtask` oder Makefile für die Gates | **`xtask`**, weil die Gates `harw-code-graph` aufrufen müssen; aus einem Makefile ginge das nur über einen zusätzlichen Binary-Wrapper. Stub in AW0-00, `Makefile` ruft es. |
| 3 | IPC Probe→Sentinel | **`SOCK_SEQPACKET`-Unix-Socket**, nicht Ringpuffer: erhält Nachrichtengrenzen, trägt `SO_PEERCRED`, kein geteilter Speicher zwischen Privilegienklassen. |
| 4 | Landlock ohne Kernelunterstützung | **Asymmetrisch**: harter Startfehler für die drei privilegierten Binaries, Degradation mit `SensorDegraded` für den Sentinel. Konservativ heißt hier nicht überall dasselbe. |
| 5 | Wo `harw-lens-rank` wohnt | **Typen ins Typ-Crate, Funktionen ins Funktions-Crate** (K11). War als „offene Frage" geführt, ist ein AW0-Blocker. |
| 6 | BM25 doppelt | **Nicht während des Programms bewegen.** `harw-knowledge` hat echtes BM25 und wird von vier Knoten geschrieben; `harw-lens-index::Bm25Index` wird eine zweite Implementierung. Bewusst geduldete Doppelung, Zusammenlegung nach AW7 bewertet. |
| 7 | `cargo-deny` / `cargo-vet` | **`cargo-deny` in AW0-00** (billig, sofort nützlich bei ~50 neuen Crates); **`cargo-vet` erst ab AW5** im DoD-Teilbaum (teuer, dort aber begründet). D8 wird bis dahin auf das reduziert, was tatsächlich läuft. |
| 8 | `Confidence`-Zusammenlegung | **Nach `harw-types`**, nicht per Reexport `model-catalog → memory` (K3, nachgeprüft: kein Zyklus, aber der Reexport zöge die ganze Memory-Crate herein). |

### Später fällig

| Frage | Wann |
|---|---|
| `rustables`-Reife (Auflage: Trait so schneiden, dass `netlink-packet-*` ein Austausch ist, keine Umschreibung) | vor AW5-04a |
| **Histogramm-Buckets** — blockiert AW3-04s Golden-Test, also **am Ende von AW1** entscheiden, nicht „irgendwann nach AW1" | Ende AW1 |
| `ChunkDigest` = `ContentDigest`, Newtype oder eigener Typ | vor AW0-08 |
| Steward-Fensterlängen | AW6-08 |
| Goldkorpus vierte Embedding-Schicht | AW7-05 |
| TUI über `harw-web` oder direkt | über das Programm hinaus |

### Verbleibende Unsicherheiten

- **Ob `ContextBudgetSpec` wirklich als Newtype über `BudgetSpec`
  funktioniert.** `ContextBudgetSpec` trägt `per_section: BTreeMap<SectionName,
  u32>`; ob `pack` dieselbe Struktur braucht oder eine flachere, geht aus den
  Plänen nicht hervor. **Vor AW0-08 gehören `harw-lens-plan.md` §5 und der
  Kontextplan §4 nebeneinander gelesen** — es ist der Vertragsknoten, an dem
  zwei Teilsysteme hängen.
- **Ob `harw-dod-bpf` (AW7-01a) besser nach AW2 gehört.** Strukturell ist es
  eine L2-Zugriffsschicht wie `readfs` und `netlink`. Dagegen: `aya` ist eine
  schwere Abhängigkeit, und die Zerlegung ist ausdrücklich stolz darauf, dass
  fünf von acht Stufen ohne Kernel-Berechtigung laufen. In AW7 belassen, die
  Gegenposition ist vertretbar.
- **Ob neun parallele Sensor-Agenten die richtige Zahl sind.** Der
  Präzedenzfall sagt, 13 sind machbar; die Sensoren sind besser geformt. Ich
  fahre trotzdem 2 + 9 statt 11, weil die Kosten eines Fehlschlags hier (elf
  neue Crates) höher sind als der Gewinn einer Ebene.

---

## Fortschritt und Korrekturen aus der Umsetzung

Fortlaufend geschrieben. Trägt, was sich am Plan als falsch erwiesen hat —
Korrekturen gehören dorthin, wo jemand sie beim Weiterarbeiten findet, nicht
in eine Commit-Nachricht.

### AW0-00 · Workspace-Fundament — abgeschlossen

- **49 neue Crates** angelegt (nicht 50), Workspace hat jetzt **95 Member**.
- `[workspace.dependencies]` mit einer `jiff`-Fassung; die 15 Bestandsnennungen
  in fünf Schreibweisen und drei Fassungen sind vereinheitlicht.
- `[workspace.lints.rust] unsafe_code = "forbid"` plus `[lints] workspace = true`
  in allen 46 Bestands- und allen 49 neuen Crates.
- `deny.toml` und `.cargo/config.toml` (Alias `cargo xtask`) angelegt.
- `xtask` mit `main.rs` (Verteilung) und je einer eigenen Datei für AW0-10
  (`gates.rs`) und UI-01 (`webui.rs`) — damit fasst keiner der beiden Knoten
  eine Datei des anderen an (K14).
- **Gates:** `cargo check --workspace` und `--all-targets` grün;
  `cargo test -p harw-secrets -p harw-browser -p harw-tools` grün.

### K23 · `harw-home` existiert bereits — AW0-11 ist eine Erweiterung

K13 führte `harw-home` unter „Crates ohne Arbeitsknoten", gestützt auf die
Architekturdokumente. **Die Crate existiert seit langem** (746 Zeilen, mit
`paths.rs`). AW0-11 legt sie nicht an, sondern ergänzt sechs Pfadnamen. Damit
sind es 49 statt 50 neue Crates.

*Warum das kein Detailfehler ist:* die Analyse prüfte gegen die **Dokumente**,
nicht gegen das Verzeichnis. Eine Aussage über den Ist-Stand wird am Ist-Stand
geprüft.

### K24 · `trait Sensor` gehört nach `harw-dod-signals`, nicht nach `harw-dod-cap`

Der Trait muss `HostSample` und `SecurityEvent` nennen; die liegen in
`harw-dod-signals`, das seinerseits an `harw-dod-cap` hängt. Ein Zyklus.

**Es gilt:** `harw-dod-cap` besitzt das **Zugriffs**vokabular (`Capability`,
`CapabilityClass`, `ReadScope`, `SensorError`, `Permanence`,
`SensorHandle<S>`). `harw-dod-signals` besitzt das **Daten**vokabular *und*
`trait Sensor` samt `SensorReading`.

Derselbe Fehlertyp wie K11 (`pack`), eine Ebene tiefer: zwei Knoten derselben
Welle, die einander definieren sollen. Beide wurden erst beim Ausformulieren
der Signaturen sichtbar — ein Argument dafür, den Contract-Master als Code und
nicht als Prosa zu führen.

### K25 · `Finding<S>` gehört vollständig nach `harw-dod-rules`

K12 stellte fest, dass `pub(crate)`-Konstruktoren in `harw-dod-signals` für elf
Sensor-Crates unerreichbar sind. Beim Ausformulieren zeigt sich der bessere
Schnitt: **`Finding<S>` und alle drei Übergänge leben in `harw-dod-rules`
(AW4-03); `harw-dod-signals` enthält kein `Finding`.**

Damit sind die Konstruktoren dort natürlich `pub(crate)`, und
`harw-dod-escalate` kommt an ein `Finding<Triaged>` **nur, indem es ein
`Finding<RuleChecked>` besitzt** — herstellbar allein durch `harw-dod-rules`.
Der Besitz des Wertes *ist* die Berechtigung. Der Typ trägt die Regel; kein
Reviewer muss sie tragen. Genau die Bewegung, die bei `can_spawn` (K20)
seinerzeit misslang: dort wurde aus einer Zusage ein Laufzeit-Check statt einer
Unausdrückbarkeit.

### K26 · Fünf `unsafe`-Stellen, nicht drei — zwei in Integrationstests

K17 zählte drei aus `src/`. Es sind fünf: dazu
`harw-browser/tests/session_lifecycle.rs:50` und
`harw-tools/tests/macros.rs:139,143`. Beide liegen in `tests/`, also in
**eigenen Crate-Wurzeln** — `#![forbid(unsafe_code)]` in `lib.rs` erfasst sie
nicht, `[lints] workspace = true` schon. `harw-tools` trug das Attribut und
hatte trotzdem `unsafe` im Testziel.

Vier waren derselbe Fall und entfielen ersatzlos: `std::task::Waker::noop()`
(stabil seit 1.85) für handgerollte No-op-Vtables, `std::pin::pin!` für
`Pin::new_unchecked`.

Die fünfte (`harw-secrets/src/kek.rs`, `std::env::set_var` im Test) brauchte
eine Naht: `env_seed_from_os_value(var: &str, value: &OsStr)` trägt die
Prüfung, `load_seed` behält den Prozesszugriff. **Nebeneffekt: ein bisher
unerreichbarer Fall wurde prüfbar** — über `set_var` mit einem `&str` ist der
Wert immer gültiges UTF-8, der Nicht-UTF-8-Zweig war nie getestet. Aus einem
Test wurden vier.

### K27 · Die `Confidence`-Zusammenlegung wird eigener Knoten (AW0-03b)

**Nachgeprüft: kein Zyklus** — `harw-model-catalog` hängt nur an `harw-config`
und `harw-types`, `harw-memory` hat null interne Kanten.

Trotzdem verlagert: AW0-03 liegt auf dem kritischen Pfad von rund
einundvierzig Crates und bleibt **rein additiv**. Eine Änderung an zwei
bestehenden, workspaceweit benutzten Enums gehört nicht in denselben Knoten.
**AW0-03b** (nach AW0-03; Schreibbereich `harw-types/src/confidence.rs`,
`harw-memory/src/epistemic.rs`, `harw-model-catalog/src/provenance.rs`) zieht
den Typ nach `harw-types`, beide reexportieren. Ein Reexport
`model-catalog → memory` wäre billiger, aber schlechter: er zöge die ganze
Memory-Crate für ein Enum herein.

### K28 · Korrigierte Ausführungsebenen von AW0

Die Ebenentabelle setzte AW0-01 und AW0-11 auf dieselbe Ebene, obwohl AW0-01
an AW0-11 hängt (der File-Sink schreibt unter das harw-Home). Ebenso hängt
AW0-04 an AW0-08 (K11). Korrigiert:

| Ebene | Knoten | Breite |
|---|---|---|
| 1 | AW0-00 | 1 |
| 2 | AW0-03, AW0-11, AW0-10 | 3 |
| 3 | AW0-01, AW0-06, AW0-08, AW0-02 | 4 |
| 4 | AW0-04, AW0-07, AW0-09, AW0-03b | 4 |
| 5 | AW0-05 | 1 |

### K29 · Plattenplatz ist die bindende Ressource, nicht die Rechenzeit

Die Partition liegt dauerhaft bei 99 % (214 von 226 GB belegt, davon nichts
aus diesem Projekt). Der Spielraum beträgt **rund 2,5 GB**. Ein
`cargo check --workspace --all-targets` erzeugt etwa 1,5 GB; ein liegen
gelassenes `target/` hat während des Zwölf-Agenten-Fan-outs den Host auf
0 MB gebracht, woraufhin auch Werkzeugausgaben mit ENOSPC scheiterten und die
Fehlermeldung auf `/tmp` zeigte, obwohl `target/` der Verursacher war.

**Regel für die Umsetzung:** während der reinen Bauphase existiert kein
`target/`. Nach jedem Verifikationslauf wird es entfernt, nicht nur
`cargo clean` aufgerufen.

### Stand der Umsetzung

**Gelandet:** AW0-00 bis AW0-09, AW0-11, AW1-01, AW1-07, AW2-03, AW2-04, AW3-01,
AW3-06, AW4-04, AW4-07 plus zwei Nachzugs-Knoten für gebrochene
Konstruktionsstellen. Rund **13.000 Zeilen** neuer Code in fünfzehn Crates.

**Neue Knoten, die während der Umsetzung entstanden:**

| Knoten | Warum |
|---|---|
| **AW1-01b** · Trace-Vererbungskette | siehe K30 |
| **AW4-08** · CIDR-Aufrufstellen | Folge von K1, war im Plan bereits vorgesehen |
| *Nachzug* `EvidenceRef.digest` | 12 Konstruktionsstellen in 3 Crates |
| *Nachzug* `StoredJob.trace` / `ChildLeaseRecord.trace` | 13 Konstruktionsstellen in 5 Crates |

### K30 · `TraceContext` erreicht nichts — die Vererbungskette fehlt

AW1-01 hat `trace: Option<TraceContext>` an `StoredJob` und `ChildLeaseRecord`
gehängt. Der Nachzugs-Knoten hat dann alle dreizehn Konstruktionsstellen
geprüft und festgestellt: **keine einzige Kontextstruktur im Baum trägt einen
Trace.** Nicht `SpawnContext`, nicht `AdmissionContext`, nicht
`McpRequestContext`, nicht `JobAdmissionTemplate`. Alle dreizehn Stellen
tragen `None`, weil es nichts durchzureichen gibt.

Das ist exakt das Muster aus [[harwness-gruene-gates-beweisen-nichts]]:
gebaut, getestet, einzeln korrekt — von niemandem erreicht. Ein Feld, das
immer `None` ist, ist von einem fehlenden Feld nicht zu unterscheiden.

**Es gilt: AW1-01b** schließt die eine Kette, die tragfähig ist —
`SpawnContext` → `ChildRecord` → `ChildLeaseRecord`, mit der richtigen
Semantik (gleiche `trace_id`, neue `span_id`, Elternteil als
`parent_span_id`). Die drei anderen Lücken bleiben offen, **weil ihre
Fassaden repoweit keinen Produktionsaufrufer haben** — ein Trace in eine
Kette zu hängen, die niemand betritt, verschöbe das Problem nur.

**Nebenbefund, Bestand:** `JobAdmissionService::admit`,
`PlanJobBridge::admit_ready_nodes` und `McpRequestContext::from_trusted_ingress`
werden repoweit **nirgends produktiv aufgerufen**. Drei unverdrahtete
Fassaden — nicht durch dieses Programm entstanden, aber durch es gefunden.

### K31 · Fünf Fehler im Contract-Master, von den Agenten gefunden

Alle fünf wurden von Agenten gemeldet statt umgangen, und alle sind zentral
korrigiert:

1. **`#[from]` auf einem benannten Feld ist wirkungslos.** Das Ableitungsmakro
   erzeugt das `From`-Impl nur variantenweise auf einem Ein-Feld-Tupel
   (`harw-macros/src/error.rs:47-52`). Auf einem benannten Feld ist es als
   Helfer-Attribut inert — es kompiliert, erzeugt still nichts, und `?`
   funktioniert nicht, ohne dass der Compiler etwas sagt.
2. **Das Makro erzeugt kein `Debug`.** Es steht überall als eigenes Derive
   daneben.
3. **`CostEstimate` und `BudgetSpec` brauchten serde.** `Fragment.cost` und
   `ContextBudgetSpec.total` serialisieren beide; ohne die Ableitung an den
   Feldtypen kompiliert `harw-context` nicht. Zwei Agenten fanden es
   unabhängig voneinander.
4. **`harw-types` benutzt kein `harw_macros::HarwId`.** Es hat ein lokales
   `macro_rules! newtype_id!`, dessen Erzeugnisse **kein `Ord`, kein
   `PartialOrd`, kein `Copy`** haben — und dessen `new()` eine zufällige UUID
   erzeugt statt aus einer Zeichenkette zu konstruieren.
5. **`metric: &'static str` machte `HostSample` unlesbar.** Ein `&'static str`
   in einem serde-Typ erzeugt `impl Deserialize<'static>`; der Typ wäre nur
   aus einer statischen Quelle lesbar gewesen, nie aus einem zur Laufzeit
   gelesenen Puffer — obwohl `SecurityEvidence` ihn auf Platte schreibt und
   zurücklesen muss. Jetzt `Cow<'static, str>`.

### K32 · `SupersededBy` ist gerichtet, `Contradicts` ist es nicht

Zwei Agenten liefen bei `EdgeIndex` auseinander: der eine baute einen
ungerichteten Kantensatz, der andere schrieb gegen
`superseded_by(chunk) -> Option<ChunkDigest>`. Der zweite hatte recht.

„A widerspricht B" gilt in beide Richtungen; „A wurde abgelöst von B" nicht.
Verlöre die Ablösung ihre Richtung, könnte `collapse` auf die **ältere**
Fassung zusammenfallen — das Gegenteil dessen, wofür es die Kante gibt.
Zentral aufgelöst: ungerichteter `HashSet` für `References`/`Contradicts`,
gerichtete `HashMap` für `SupersededBy`.

### K33 · Zwei Integrationsdefekte in `harw-dod-cap`, von Konsumenten gemeldet

- `SensorError::Io` war eine Struct-Variante mit wirkungslosem `#[from]`;
  `harw-dod-readfs` und die elf Sensoren brauchen das `From`-Impl für `?`.
  → Tupel-Variante.
- `ReadScope` hatte keinen Zugriff auf seine Wurzeln. `allows` beantwortet
  „liegt dieser Pfad drin", nicht „welche Pfade gibt es" — ein Glob, der
  durchlaufen muss, braucht einen Startpunkt, sonst beginnt er bei `/` und
  liest unterwegs Verzeichnisse, die ihn nichts angehen. → `roots()`.

### K34 · Der Plattenengpass hat sich entspannt

Zum Zeitpunkt von K29 waren 2,5 GB frei; inzwischen sind es **16 GB**. Eine
Schutzschleife läuft mit und entfernt `target/`, falls der freie Platz unter
900 MB fällt — sie hat bisher nicht eingegriffen. Das vorhandene `target/`
gehört rust-analyzer, dessen laufende Diagnostik während der Bauphase
kostenlose Zwischenprüfungen liefert.

### K35 · Ein echter Sicherheitsfehler im Bestand: Rückverweise umgingen die Sichtbarkeit

`VisibilityScope::OperatorOnly` **wurde** in `harw-knowledge` beim direkten
Kandidatenfilter von `search_with` durchgesetzt. Aber `expand_backlinks` —
die Breitensuche über Palace-Rückverweise — rief `index.backlinks()` auf und
nahm **jedes** verlinkende Artefakt in die Ergebnismenge, ohne dessen eigene
Sichtbarkeit zu prüfen.

Folge: ein `SelfOnly`-, `DescendantTree`-, `ExplicitlyGranted`- oder
`OperatorOnly`-Artefakt, das bloß auf einen sichtbaren Treffer verlinkte oder
von ihm verlinkt wurde, erschien im Abruf — obwohl `visible_to_caller`
dokumentiert, dass diese Bereiche für jeden Aufrufer fail-closed sind.

Gefunden, weil der Auftrag ausdrücklich verlangte, **das Verhalten** zu prüfen
statt die Dokumentation. Der Agent schrieb erst den fehlschlagenden Test
(privates `SelfOnly`-Tagebuch verlinkt auf einen `OperatorOnly`-Befund), dann
den Fix: `visible_to_caller` wird bei **jedem Sprung** neu geprüft, und die
Traversierung endet an allem Unsichtbaren.

Das ist derselbe Fehlertyp wie K20 (`can_spawn`): eine Zusage, die an einer
Stelle durchgesetzt wird und an einer zweiten nicht.

### K36 · `validate_patch` hat keinen Aufrufer — und deshalb keine Metrik

Der Metrik-Knoten sollte acht Messgrößen der Planschleife emittieren. Sieben
sind gebaut. Die achte, `scope_violation_rate`, **wurde bewusst weggelassen**:
`harw_plan::admission::validate_patch` — die Funktion, die einen Schreibversuch
außerhalb des Reviers ablehnt — hat workspace-weit **keinen einzigen Aufrufer**
außerhalb ihres eigenen Testmoduls. Auch `harw-plan-bridge`, das den
`MutationContract` für den Job-Payload baut, ruft sie nirgends auf.

Eine Metrik daran hätte dauerhaft null gezeigt, und null wäre als „keine
Verletzungen" gelesen worden — nicht als „nie geprüft". Der Agent hat sie nicht
gebaut und den Grund im Modul dokumentiert.

**Das ist ein offener Befund, kein erledigter:** die Revierprüfung ist gebaut,
getestet und wird nicht aufgerufen. Sie gehört auf dieselbe Liste wie die drei
unverdrahteten Fassaden aus K30.

*Nebenbefund:* die Job-Rückkanäle `on_job_completed`/`on_job_failed` sind
verdrahtet, aber ohne HARW-Home oder mit `[tools.plan] enabled = false`
blockiert der Worker jeden `plan-node`-Job dauerhaft, statt ihn abzuschließen
oder scheitern zu lassen.

### Verifikationsablauf, wie er tatsächlich gefahren wird

Die Bauphase läuft auf ausdrückliche Anweisung vollständig parallel; geprüft
wird erst danach, sequenziell und an einer Stelle. Der Ablauf:

1. **`cargo check --workspace`** — nur Lib- und Bin-Ziele. Die schnellste
   Stufe und die, die Vertragsdrift zwischen Crates findet. Erwartet wird eine
   dreistellige Fehlerzahl; das ist der eingeplante Preis der Parallelität.
2. **Fehler in Gruppen beheben**, nach Ursache sortiert statt nach Datei. Aus
   dem Vorprogramm ist bekannt, dass die Fehler sich zu wenigen Ursachen
   bündeln — 26 Fehler waren dort **eine** Vertragsdrift.
3. **`cargo check --workspace --all-targets`** — fängt, was erst in Tests
   driftet. Im Vorprogramm fiel genau hier ein Feldfehler auf, den Stufe 1
   nicht sah.
4. **`TRYBUILD=overwrite cargo test -p harw-macros`** — die vier Makro-Knoten
   haben ihre `.stderr`-Erwartungen nach bestem Wissen geschrieben, ohne sie
   ausführen zu können. Das Nachziehen ist eingeplant, kein Fehler.
5. **`cargo clippy --workspace --all-targets --all-features -- -D warnings`**
6. **`cargo test --workspace`**
7. **Bottom-up-Analyse mit Plan-Abgleich** — mit der vierten Spalte
   „Konsument existiert?", geprüft per Suche nach Aufrufen außerhalb der
   eigenen Crate und außerhalb von Tests. Beide Gates grün beweisen nicht,
   dass Code angeschlossen ist.

**Bereits bekannte Fundstellen für Schritt 7**, gemeldet von Agenten während
des Bauens und noch offen:

| Befund | Quelle | Stand (nachgeprüft) |
|---|---|---|
| `TraceContext` hat keinen Wurzel-Erzeuger | AW1-01b | **erledigt** — `harw-cli/src/chat.rs::new_one_shot_root_trace` und das Gegenstück in `job_worker.rs` erzeugen die Wurzel; `child_controller.rs:120` leitet Kindspannen daraus ab. Die Kette ist von einem echten Eintrittspunkt aus geschlossen. |
| `ContextProgram` hat keinen öffentlichen Konstruktor | AW1-03 | **verschoben, nicht geschlossen** — `ContextProgram::from_resolved_program` existiert (`executable.rs:746`). Aber **jede** Aufrufstelle liegt in `#[cfg(test)]` oder in einem Doc-Beispiel derselben Crate. Der Konstruktor fehlt nicht mehr; der Aufrufer fehlt weiterhin. |
| `validate_patch` hat workspace-weit keinen Aufrufer | AW1-05 | **steht, und der Grund ist tiefer** (K66): nicht der Aufrufer fehlt, sondern die **Eingabe** — nirgends im Baum entsteht ein `UnifiedDiff`. Die richtige Stelle ist benannt: `harw-cli/src/job_worker.rs`, zwischen Turn-Ende und `report_plan_node_outcome`. |
| `JobClaim` trägt `StoredJob.trace` nicht weiter | Nachzugs-Knoten | **steht, ausführlich belegt** — `job_worker.rs:376-392` dokumentiert, dass ein frisch erfundener Trace *schlechter* wäre als keiner (er behauptete eine Verwandtschaft, die nicht besteht), gibt `None` zurück und hat den Parameter bereits durchgereicht, damit ein künftiges `JobClaim.trace` nur noch anzuschließen ist. |
| `JobAdmissionService::admit`, `admit_ready_nodes`, `McpRequestContext::from_trusted_ingress` — drei unverdrahtete Fassaden | Nachzugs-Knoten | steht (Bestand, nicht durch dieses Programm entstanden) |
| `MetricKey` trägt keinen Beschreibungstext, `# HELP` bleibt leer | AW3-04 | steht |
| `sensor_suite!` braucht einen zweiten Fixture-Begriff für Backend-Sensoren | K41, K48 | **steht** — K48 hat Bereichsdichtheit und „leere Quelle" getrennt, das filesystem-zentrierte Fixture-Modell bleibt |
| `DetailMode::References` erreicht die Turn-Schleife nicht | AW5-07 | **weitgehend geschlossen** (K70, K75): `ContextProgram` trägt den `DetailMode`, `ToolExecutor::as_context_load_executor` wird in `turn_loop` aufgerufen, `contribute_v2` hat einen Produktionsaufrufer, und `harw-core/tests/detail_mode_references.rs` belegt die Auflösung über `context.load`. **Offen bleibt** der Anschluss der Montage: `Assembly::gather` hat weiterhin keinen Produktionsaufrufer, weil `gather_context` sofort auf `ContextFragment` zurückwandelt — nötig wäre `ModelRequest::with_context_budget` in `harw-core/src/model.rs`. |
| `harw-dod-flow` implementiert `Sensor` nicht; der einzige Konsument musste einen Adapter bauen | K52 | **behoben** (K72): `FlowSensor` implementiert `Sensor`, `observe()` bleibt als Ein-Ereignis-Schritt, aus dem `poll()` gebaut ist. Der Adapter in `harw-probe-bpf` ist dadurch überflüssig, wurde aber nicht entfernt. |
| `harw-dod-procmon` exportiert keine zu `flow_program_spec()` symmetrische Konstante | AW7-01d | **behoben** (K72): `PROCMON_TRACEPOINT_ATTACH_POINT` und `procmon_program_spec(...)`. |
| `SnapshotId` hat weder `Serialize`/`Deserialize` noch einen öffentlichen Konstruktor | AW5-09 | **behoben** (K76) über zwei Typen: `SnapshotId` bleibt berechnet und nur intern konstruierbar, `ReferencedSnapshotId` ist die einlesbare **Behauptung**, die erst `confirm()` in eine Identität verwandelt. Die Domänenfassung ist ein sichtbares Serde-Feld. |
| Eine Rolle kann einer Familie nicht beitreten, ohne die Familiendatei zu ändern — AW6-00s Entkopplung deckt nur Auffindbarkeit, nicht Aufnahme | AW6-01 | in Arbeit |
| `warden_actions!` hatte null Konsumenten, weil es `harw-tools` bedingungslos hereinzog | AW5-02 | **behoben** — `tool_schema` ist durch Weglassen abwählbar |
| `/context-proposal` war gebaut und nirgends registriert | AW5-09 | **behoben**, plus ein Vollständigkeitstest, der die Klasse künftig fängt |
| `EventKind::StructureDrift` passt nur halb auf einen Scanner-Befund | K44 | bewusst offen |
| `harw-dod-readfs` bietet kein Symlink-Lesen und keine Verzeichnisauflistung | zwei Sensorknoten | steht, mit dokumentierten Ausnahmen |
| **`harw-web` liefert die `TrustClass` nicht** — weder `WebEvent` noch `OpOutput` tragen ein Klassenfeld. Die Zwei-Block-Trennung aus AW4-01 ist in der UI damit **unsichtbar**; die CSS-Token liegen bereit und sind unbenutzt | UI-01 | **steht** — und wiegt schwer: die Trennung wirkt serverseitig, aber der Bediener sieht nicht, was er vor sich hat |
| Der Typgenerator liest die Routen **textuell** aus dem Rust-Quelltext, weil `xtask` nicht gegen `harw-ops` linken darf (das zöge halbe Business-Logik in eine schlanke Werkzeug-Crate) | UI-01 | steht — Vorschlag: `syn` gezielt in `xtask`, oder ein JSON-Dump-Binary in `harw-ops` |
| `#[context_provider]` erzeugt keine `ContextProvider::namespace()`/`max_trust()`-Überschreibungen | Trait-Knoten | **behoben** (K67), und die Vorgabe lag **strenger** als jede Deklaration, nicht lockerer. Neuer Befund an derselben Stelle: es gibt workspace-weit **genau eine** produktive Anwendung des Makros, und die deklariert nichts. |
| Von **33** `EvidenceRef`-Konstruktionsstellen setzen **zwei** einen Digest, fünfzehn ausdrücklich `None` — `TrustClass::Evidence` ist damit praktisch unerreichbar | K54, nachgemessen | **steht, quantifiziert** |
| Die `[context]`-Grammatik erzwingt den Schnitt bei `extends` nicht — Sektionen werden additiv verschmolzen, `MergeOp::Intersect` gibt es nur für `patch.exclude.*` | AW6-05 | in Arbeit |
| Ein `toml`-Beispiel in `harw-agent-dsl/src/context_program.rs` ist falsch (`exclude` nach den `[[sections]]`) und **kann nicht fehlschlagen**, weil es kein Doctest ist | AW6-05 | in Arbeit |

**Die Unterscheidung in Zeile zwei ist der Grund, warum diese Tabelle eine
dritte Spalte hat.** „Konstruktor fehlt" und „Aufrufer fehlt" sehen im
Fortschrittsbericht gleich aus — *ein Knoten hat etwas gebaut* — und sind
verschieden weit vom Ziel entfernt. Ein Befund, der sich verschiebt, darf
nicht als geschlossen gezählt werden.

Der Plattenengpass aus K29/K34 ist entschärft (16 GB frei), ein
`--all-targets`-Lauf am Stück ist wieder möglich. Die Schutzschleife läuft
weiter mit.

### K37 · `NetworkScope` hatte keinen verlustfreien Zugriff — zwei Konsumenten stolperten darüber

`EgressTarget` kam mit K1 in `harw-sandbox`. Was fehlte, fiel erst auf, als
zwei Knoten unabhängig voneinander darauf zugreifen wollten:

- **`hosts()` ist verlustbehaftet** — es klappt `Host` und `DnsSuffix` zu
  nackten Namen zusammen und lässt `Cidr` **ganz weg**. Für eine
  Diagnoseausgabe genügt das; für einen Aufrufer, der aus dem Scope einen
  Netzplan **ableiten** muss, nicht.
- **Es gab keinen Konstruktor für `Host` oder `Cidr`** — nur `from_hosts`
  (das immer `DnsSuffix` erzeugt) und `Deserialize`.

Der Netzplan-Knoten (AW3-02) hat sich deshalb über die **Serde-Form**
beholfen: Scope serialisieren, `allow_hosts` lesen, je Eintrag wieder als
`EgressTarget` deserialisieren. Das funktioniert und ist sogar sorgfältig
begründet — aber es benutzt eine Wire-Darstellung als Zugriffsweg und koppelt
damit an ein Format, das für Bestandsdateien stabil bleiben muss, nicht für
Programmierer.

Schlimmer: weil `NetworkScope::allows` die **Punktgrenzen-Regel** nicht
exportierte, hat derselbe Knoten sie ein zweites Mal implementiert — mit einem
ehrlichen Hinweis in der Doku, dass die beiden Kopien nur durch Property-Tests
zusammengehalten werden. Eine Sicherheitsregel in zwei Kopien driftet.

**Zentral aufgelöst:** `EgressTarget::matches_host` und `matches_addr` sind
jetzt öffentlich und tragen die Regel **einmal**; `NetworkScope::allows` und
`allows_addr` rufen sie auf. Dazu `NetworkScope::targets()` (verlustfrei,
geliehen, unveränderlich) und `from_targets(...)`.

**Das ist kein `add`:** ein Scope entsteht weiterhin einmal aus einer fertigen
Menge und kann auf keinem Weg wachsen.

### K38 · Ein Knoten war zu groß für einen Agenten

Der xtask-Gates-Knoten (AW0-10) sollte drei unabhängige Prüfungen samt
Markdown-Zerlegung des Plans in **einer** Datei bauen. Der Agent hat sein
Ausgabebudget verbraucht, **ohne eine Zeile zu schreiben**.

Aufgeteilt nach demselben Muster, das bei `xtask/src/main.rs` funktioniert
hat: ein Verteiler (`gates.rs`, von mir) plus je eine Datei je Gate —
`gate_edges.rs` (AW0-10a), `gate_privileges.rs` (AW0-10b),
`gate_writescopes.rs` (AW0-10c). Drei Besitzer, drei Dateien, keine geteilte.

Jedes Gerüst meldet ausdrücklich `Err("noch nicht gebaut")` statt grün mit
null geprüften Kandidaten. Ein ungebautes Gate, das grün meldet, ist genau die
Verwechslung, gegen die die `GateReport::checked`-Zahl eingeführt wurde.

### K39 · `sensor_suite!` passt nicht auf jeden Sensor

Der Harness verlangt `From<SensorHandle<Bound>>` als **einzigen**
Konstruktionsweg. `WorkspaceDriftSensor` braucht aber zusätzlich ein
`baseline: Option<Inventory>` — das ist der ganze Zweck eines Driftsensors und
aus einem bloßen Griff nicht ableitbar.

Der Agent hat es **nicht** erzwungen und begründet, warum: ein `From`-Impl mit
`baseline: None` ließe jede der sechs Prüfungen **leer durchlaufen** (null
Ereignisse, immer grün), ohne die Drifterkennung je auszuführen — und ein
späterer Aufrufer, der `.into()` benutzt, bekäme still nie einen Bericht.

Offen: `sensor_suite!` sollte einen Konstruktor-Ausdruck entgegennehmen statt
`From` zu verlangen. Betrifft nur Sensoren mit Konfiguration jenseits des
Griffs; die sieben Strom-A-Sensoren sind nicht betroffen.

### K40 · Eine Sicherheitsregel las ihre Schwere aus Prosa — behoben

`EventKind::StructureDrift` trug in meiner Vertragsfassung **nur** einen
Freitext. Der Workspace-Sensor hatte die Schwere strukturiert
(`VersionSeverity` mit SemVer-Feinheiten), flachte sie in seiner privaten
`describe`-Funktion zu Prosa ab — und die Regel gewann sie über feste
Textpräfixe (`"Versionssprung ("`, `"neue Abhängigkeit:"`, …) wieder heraus.

**Eine Sicherheitsregel, die ihre Einstufung aus Textmustern rekonstruiert.**
Eine geänderte Formulierung im Sensor hätte sie still danebengreifen lassen:
kein Compilefehler, kein fehlschlagender Test, nur ein Befund, der ab dann
falsch eingestuft ist.

Der erbauende Knoten hat die Kopplung **offengelegt statt hingenommen** und
den richtigen Fix benannt. Umgesetzt:

- `harw-dod-signals` bekommt `DriftSeverity { Unknown, Low, Medium, High }`,
  bewusst grob und quellenunabhängig.
- `EventKind::StructureDrift` trägt das Feld, mit `#[serde(default)]`, damit
  Bestandsdatensätze lesbar bleiben — sie fallen auf `Unknown`.
- **`Unknown` ist nicht `Low`.** Wer eine unbekannte Schwere wie eine geringe
  behandelt, spricht einen Befund frei, über den nie jemand entschieden hat.
  Die Regel bildet `Unknown` deshalb auf `Severity::Medium` ab.
- Der Sensor bildet seine feinere Einstufung an **genau einer Stelle** ab.
- Ein Regressionstest hält fest, dass **derselbe Schweregrad mit völlig
  verschiedenem Freitext dieselbe Einstufung** ergibt — er schlüge fehl, wenn
  jemand die Textabhängigkeit wieder einführt.

### K41 · `sensor_suite!` braucht einen zweiten Fixture-Begriff, nicht nur einen anderen Konstruktor

K39 stellte fest, dass der Harness `From<SensorHandle<Bound>>` als einzigen
Konstruktionsweg verlangt und Sensoren mit zusätzlicher Konfiguration deshalb
nicht trägt. **Das war zu kurz gedacht.**

Der `authlog`-Knoten hat die `harness::assert_*`-Funktionen gelesen und
gefunden: sie sind bereits generisch über `F: Fn(SensorHandle<Bound>) -> S`,
also nicht an `From` gebunden. Das Problem liegt tiefer — **das ganze
Fixture-Modell setzt einen dateisystemlesenden Sensor voraus**: `ReadScope`
auf `fixtures/<fall>/tree`, Kanarien-Injektion in Dateien, Scope-Dichtheit
über ein leeres Verzeichnis.

Ein Sensor, der über ein injiziertes Backend liest und `handle.scope()` nie
anfasst — `AuthlogSensor` über `AuditSource`, künftig die eBPF-Sensoren über
`BpfLoader` —, **besteht jede der sechs Prüfungen trivial**, ohne seine
Parselogik je auszuführen. Grün, weil nichts geprüft wurde: genau die
Verwechslung, gegen die die Prüfungen gebaut wurden.

**Offen:** `harw-dod-fixtures` braucht einen zweiten Fixture-Begriff —
Backend-Fixtures statt Baum-Fixtures. Betrifft `authlog`, `procmon`, `flow`
und den Drift-Sensor.

### K42 · Der Glob im Sensor-Makro war zur Compile-Zeit fest — und damit im Test blind

Das Ableitungsmakro `#[derive(SensorSource)]` verdrahtete sein Suchmuster als
**vollen Pfad zur Compile-Zeit**. `harw_dod_readfs::glob::glob` sucht aber ab
dem echten `/`; der `ReadScope` wirkt nur als nachträglicher Filter.

Folge: ein so erzeugter Sensor findet **in Produktion** etwas — die
Bereichswurzel ist zufällig derselbe Pfad — und **in der Fixture-Prüfung
nichts**, weil die Wurzel dort `fixtures/<fall>/tree` ist. Das Ergebnis wäre
kein Fehler, sondern ein still leeres `SensorReading`, und **alle sechs
Harness-Prüfungen meldeten grün, ohne je etwas ausgeführt zu haben.**

Gefunden vom seriellen sysfs-Validierungssensor, bevor die fünf parallelen
starteten. Wären sie gleichzeitig gelaufen, hätten sieben Crates denselben
Defekt getragen.

**Behoben:** `#[source(glob = "…")]` ist jetzt ein **Suffix relativ zur
Bereichswurzel**; der erzeugte `poll` baut den vollen Pfad zur Laufzeit aus
`scope.roots().next()`. Ein Bereich ohne Wurzel liefert eine leere
Trefferliste, keinen Fehler.

*Das ist die Rechtfertigung für den seriellen Zwischenschritt* — zwei Sensoren
vor fünf zu bauen kostete zwei Knoten Laufzeit und hat fünf Crates vor einem
stillen Defekt bewahrt.

### K43 · Eine Prüfung, die tautologisch gegen ihre eigene Quelle vergleicht

`harw-lens-index` dokumentiert: „eine Abfrage gegen ein abweichendes Modell
oder eine abweichende `chunker_version` wird abgelehnt, nie stillschweigend
beantwortet". Die Prüfung ist gebaut und hat einen grünen Unit-Test.

**Sie war über den öffentlichen Weg unerreichbar.** `harw_lens_query::query`
baute sein `Query.manifest` aus `index.manifest().clone()` — also aus genau
dem Index, den es durchsuchte. `IndexManifest::compatible_with` verglich damit
den Index mit sich selbst und konnte nie fehlschlagen.

Der Unit-Test bestand, weil er sich sein abweichendes `Query` **von Hand**
baute. Kein echter Aufrufer konnte diesen Zustand erzeugen.

**Warum das zählt:** ein Index, der mit Modell A gebaut und mit Vektoren aus
Modell B abgefragt wird, liefert Treffer. Sie sind Unsinn, aber sie sehen aus
wie Treffer.

**Behoben:** `query`/`query_scoped`/`ask` nehmen jetzt ein
`QueryProvenance { model, chunker_version }` — der Aufrufer **muss** sagen,
womit er eingebettet hat. Das Abfrage-Manifest entsteht daraus, nicht aus dem
Index. Drei Tests belegen, dass die Prüfung über den öffentlichen Weg
auslösbar ist: abweichendes Modell, abweichende Zerlegungsfassung, und ein
Gegentest mit passender Provenienz.

**Der Fehlertyp verdient einen Namen** und gehört auf die Prüfliste der
Bottom-up-Analyse: *eine Prüfung, deren Vergleichswert aus derselben Quelle
stammt wie der geprüfte Wert.* Weder Test noch Lint findet das — nur die
Frage „wer ruft das mit welchen Werten auf".

**Zweite Fundstelle, unabhängig, in einem anderen Teilsystem.** Der Knoten, der
`/context-proposal` in die Registry eingehängt hat, wurde nebenbei gefragt,
warum die fehlende Registrierung überhaupt unentdeckt bleiben konnte. Seine
Antwort:

> `register_all_adds_nineteen_operations` prüfte die Länge des Arrays **gegen
> sich selbst**. Es konnte eine Operation, die nie ins Array aufgenommen wurde,
> nicht finden — das Array **ist** definitionsgemäß das, was hineingeschrieben
> wurde.

Der Test war intern widerspruchsfrei, grün, und hat nie mit der Wirklichkeit
verglichen. Behoben durch
`every_declared_operation_struct_is_registered_somewhere`: es zählt die
`#[operation(...)]`-Deklarationen des Crates **unabhängig** auf und prüft die
Registry dagegen — der Vergleichswert kommt jetzt aus einer anderen Quelle als
der geprüfte Wert.

**Zwei Fundstellen in verschiedenen Teilsystemen machen aus einer Anekdote ein
Muster.** Die Frage für die Abnahme lautet deshalb nicht nur „gibt es einen
Konsumenten", sondern zusätzlich: **„woher stammt der Vergleichswert dieser
Prüfung?"** Eine Zahl, die neben der Liste steht, die sie zählt, ist keine
Prüfung — sie ist eine Wiederholung.

*Nebenbefund desselben Knotens:* `compact::CompactOperation` ist ebenfalls
nicht registriert, aber **kein** Versäumnis — sie ist ausdrücklich durch
`UnavailableCompactOperation` ersetzt, solange die Sitzungsmutation nicht
gebaut ist, und die eigene Moduldoku sagt das. Der neue Vollständigkeitstest
führt sie als benannte Ausnahme, nicht als stille Lücke. **Ein ungenutztes
Element ist nicht automatisch ein Versäumnis** — genau die Unterscheidung, die
der Verifikationsplan für Schritt 7 verlangt.

### K44 · `EventKind::StructureDrift` ist für einen Scanner-Befund nur halb richtig

`harw-dod-scanreport` bildet SARIF- und `cargo audit`-Befunde auf
`EventKind::StructureDrift` ab. Der erbauende Knoten hat die Frage
**ausdrücklich beantwortet statt stillschweigend entschieden**: für einen
`cargo audit`-Befund passt es (eine verwundbare Abhängigkeit *ist*
abgedriftete Struktur), für einen SARIF-Codebefund nicht.

`EventKind` ist geschlossen und hat keine Scanner-Variante. **Offen:** ob eine
`ScanFinding`-Variante nötig ist, die zusätzlich Werkzeug und Regelkennung
trägt. Bewusst nicht während der Bauphase entschieden — die Fehlbelegung ist
dokumentiert, nicht still, und nichts ist dadurch kaputt.

### K45 · Die Diagnostik von rust-analyzer taugt in dieser Bauphase nicht als Prüfung

In K34 steht, das mitlaufende `target/` von rust-analyzer liefere „kostenlose
Zwischenprüfungen". **Das ist zu stark formuliert und in dieser Phase falsch.**

Nachgeprüft an einem konkreten Fall: die Diagnostik meldete in
`harw-tool-web/src/fetch.rs` an sieben Stellen `expected String, found str`.
Nachgelesen: `validate_target` liefert `WebToolResult<String>`, und alle drei
betroffenen Fehlervarianten deklarieren `host: String`. Die Meldung ist mit
dem Dateiinhalt unvereinbar.

Der Grund ist strukturell, nicht zufällig. Zwei Ursachen überlagern sich:

1. **`harw-macros` wird über vier Wellen hinweg umgeschrieben** (AW0-02,
   AW1-02, AW2-06, AW5-01). Der Proc-Macro-Server meldet durchgehend
   `proc-macro not yet built`. Damit existiert `Display` für **keinen** der
   rund fünfzig `#[derive(HarwError)]`-Typen — und jede Zeile
   `error = %err` erzeugt eine Folgemeldung `non-primitive cast:
   &DisplayValue<&XError> as &dyn Value`. Diese Meldungen sind Kaskade, nicht
   Befund. Da **20 Crates** an `harw-macros` hängen (K18), ist das kein
   Randfall, sondern der Normalzustand der Bauphase.
2. **Dateien werden während der Analyse geschrieben.** Ein Agent, der gerade
   an einer Datei arbeitet, erzeugt Zwischenzustände, die nie kompilieren
   sollten und über die niemand etwas melden muss.

**Es gilt:** Die Diagnostik zeigt an, *dass* an einer Datei gearbeitet wird —
nicht, *ob* sie richtig ist. Eine gemeldete Abweichung wird gegen den
Dateiinhalt geprüft, bevor sie irgendjemandem zugestellt wird; ungeprüft
weitergereicht kostet sie einen Agenten Arbeitszeit für ein Phantom.

Verwertbar bleiben zwei Klassen, weil sie nicht vom Makro abhängen:
`unresolved module, can't find module file` und `unlinked-file` — beide sagen
etwas über die Modulstruktur und stimmen.

**Nachtrag, und er korrigiert den Satz, der hier zuerst stand.** Die
ursprüngliche Fassung schloss mit „alles Typbezogene ist bis zum zentralen
`cargo check` unbrauchbar". **Das war zu breit gezogen, und die Pauschalisierung
hat einen echten Fehler länger stehen lassen, als nötig gewesen wäre.**

`harw-sentinel/src/ipc.rs` meldete durchgehend
`expected (usize, usize), found i32`. Ich hatte das unter dieser Regel
abgetan. Es war echt. Gegen die **gepinnte** Quelle geprüft —
`rustix-1.1.4/src/net/send_recv/mod.rs:62` —:

```rust
pub fn recv<Fd: AsFd, Buf: Buffer<u8>>(
    fd: Fd, mut buf: Buf, flags: RecvFlags,
) -> io::Result<(Buf::Output, usize)>
```

Ein **Tupel**; der Code behandelte es als Skalar. Zwei Agenten hatten die Datei
von Hand gegengelesen und es beide übersehen — beide hatten die Signatur
angenommen statt nachgeschlagen.

Der Fehler hat einen zweiten Boden: bei gesetztem `TRUNC`-Flag gibt `recv` die
**ungekürzte** Länge zurück, während es intern auf die Puffergröße kappt.
`&buffer[..recv_len]` greift dann über die Grenze und panickt — bei einer
Nachricht, die größer ist als der Empfangspuffer, also **von der Gegenseite
steuerbar**.

**Es gilt die engere Regel:** eine Meldung, die *nicht* dem
`Display`-Kaskadenmuster folgt **und** eine Bearbeitung der Datei überlebt,
wird gegen die gepinnte Abhängigkeitsquelle unter
`~/.cargo/registry/src/` geprüft — nicht gegen docs.rs, nicht aus dem
Gedächtnis, und nicht pauschal verworfen.

**Und ein Umstand hat sich geändert:** der Proc-Macro-Server hat `harw-macros`
inzwischen gebaut, und seither kommen Meldungen mit der Quelle `(rustc)` statt
`(rust-analyzer)`. Die sind belastbar — sie haben unmittelbar einen
halbfertigen Umbau an `contributor.rs` sichtbar gemacht (fehlender
`Ident`-Import, ein Eintrittspunkt, der noch den alten Argumenttyp weiterreicht),
der zwanzig abhängige Crates blockiert hätte. **Die Herkunftsangabe der
Meldung ist das Unterscheidungsmerkmal, nicht ihr Inhalt.**

*Der allgemeine Punkt:* ein Prüfinstrument, dessen Fehlalarmquote man nicht
kennt, ist kein Prüfinstrument. Das ist derselbe Einwand, den K38 gegen ein
Gate erhoben hat, das grün mit null geprüften Kandidaten meldet — nur mit
umgekehrtem Vorzeichen.

### K46 · `allows` und `allows_addr` sind zwei Achsen, keine zwei Gründlichkeitsstufen

K1 hat `NetworkScope` um `EgressTarget::Cidr` erweitert und `allows_addr`
hinzugefügt. Der Knoten, der die drei Aufrufstellen migrieren sollte, hat
festgestellt, dass die naheliegende Migration **jeden Abruf abgelehnt hätte**:

`harw-tool-web` bekommt seinen `NetworkScope` ausnahmslos über `from_hosts`.
Der enthält damit ausschließlich `DnsSuffix`-Ziele und **kennt überhaupt keine
Adressen**. `allows_addr` lieferte für jede reale Adresse `false` — nicht weil
ein Angriff vorläge, sondern weil die Menge leer ist, gegen die geprüft wird.

**Das ist die Falle einer erweiterten API:** die neue Prüfung *sieht*
gründlicher aus als die alte und beantwortet eine andere Frage. Wer sie
„sicherheitshalber" zusätzlich einbaut, macht das Werkzeug unbrauchbar, nicht
sicherer.

**Es gilt:** `allows` ist namensgebunden und bleibt der Prüfweg für alles, was
seinen Scope aus Hostnamen bezieht. `allows_addr` gilt nur für Scopes, die
tatsächlich `Cidr`-Ziele tragen. Beide sind in `harw-sandbox` dokumentiert;
die drei Aufrufstellen tragen die Begründung jetzt am Ort.

*Nebenbefund, offen:* `reqwest::Response::remote_addr()` existiert, die
tatsächlich verbundene Adresse wäre also greifbar. Ein echter Schutz gegen
DNS-Rebinding bräuchte aber einen Resolver, der **vor** dem Verbindungsaufbau
prüft — eine Architekturentscheidung, die der Knoten zu Recht nicht im
Alleingang getroffen hat.

### K47 · Der Schreibbereich von UI-00 war zu eng, und die Zahl dazu war falsch

Die UI-Tabelle gibt UI-00 den Bereich `harw-web/**`, `harw-operations/**`. Beim
Vergeben nachgemessen:

| | Zahl |
|---|---|
| `Surface::`-Fundstellen im Baum | **218** |
| davon erschöpfende `match`-Arme (brechen bei neuer Variante) | **8** |
| davon `matches!`/`if let` (brechen nicht) | 22 |
| Rest: Konstruktionsstellen in `operations!`-Deklarationen | ~188 |

**Fünf der acht Arme liegen außerhalb des vorgesehenen Bereichs** —
`harw-tui/src/registry.rs` (2), `harw-ops/src/explore.rs` (2),
`harw-ops/src/research.rs` (1). UI-00 wäre mit seinem deklarierten Bereich
nicht kompilierbar gewesen.

**Es gilt:** UI-00 bekommt zusätzlich genau diese drei Dateien, dateigenau
benannt — nicht die Crates. `harw-ops` wird gleichzeitig von AW5-09
geschrieben, und die Bereiche bleiben nur dann disjunkt, wenn die Grenze auf
Dateiebene gezogen wird.

**Zwei Lehren, und die zweite ist die wichtigere:**

1. Die Zahl „~197 betroffene Stellen" war eine Schätzung aus einer rohen
   Textsuche. Nachgemessen sind es acht. Eine Schätzung, die eine Aufgabe
   fünfundzwanzigmal größer aussehen lässt, verzerrt die Wellenplanung.
2. **Die 22 nicht erschöpfenden Stellen sind die eigentliche Arbeit.** Ein
   `matches!(surface, Surface::ModelTool { .. })`, das „ist das vom Modell
   aufrufbar" meinen soll, wird durch eine neue Variante **stillschweigend
   falsch** — kein Compilerfehler, kein Test. Die acht Arme findet der
   Compiler; die 22 findet nur jemand, der sie einzeln liest.

### K48 · `assert_scope_tightness` misst zwei Behauptungen in einer Zusicherung

Dritter unabhängiger Befund am selben Harness (nach K39 und K41) — und der
erste, der einen Test **tatsächlich rot** macht statt ihn leer durchlaufen zu
lassen.

`sensor_suite!` erzeugt bedingungslos einen Test, der einen `ReadScope` auf ein
leeres Verzeichnis richtet und `Err(SensorError::SourceUnavailable)` fordert.
Für `harw-dod-gpu` ist das falsch: ein Host ohne GPU liefert legitim
`Ok(leer)`, und die beiden Eingaben sind von innerhalb `poll()` **nicht
unterscheidbar** — beide sind ein echtes, listbares, leeres Wurzelverzeichnis.

Der erbauende Knoten hat nach Auftrag gebaut, den Konflikt in seiner Moduldoku
festgehalten und gemeldet, dass der Test fehlschlagen **wird**, statt ihn
durch ein falsches `Err` grün zu machen. Das war richtig.

**Die Diagnose:** „Bereichsdichtheit" und „leerer Bereich ergibt einen Fehler"
sind zwei Behauptungen. Die eigentliche Frage lautet *liest der Sensor etwas
außerhalb seines `ReadScope`?*; das `Err` auf einem leeren Verzeichnis ist nur
ein **Stellvertreter** dafür — und einer, der genau dann etwas anderes misst,
wenn die Quelle legitim fehlen darf.

Bemerkenswert: der Stellvertreter hätte **K42 nicht gefunden** (den Glob, der
ab `/` suchte und den Bereich nur nachfilterte), weil ein leeres
Wurzelverzeichnis dort trotzdem nichts geliefert hätte. Eine Dichtheitsprüfung,
die den Bereich in einen Baum mit echten Quelldateien legt, hätte ihn gefunden.

**Gemeinsamer Nenner von K39, K41 und K48:** der Harness kodiert die Annahmen
des ersten Sensors, gegen den er geschrieben wurde. Jeder Sensor mit einer
anderen Quellform stößt an eine andere Kante — und **alle drei Male hat der
Sensor-Knoten es gemeldet statt umgangen.** Das ist die Auftragsformulierung,
die trägt: melden ist erlaubt und erwünscht, umgehen nicht.

### K49 · Drei von vier Stores sicherten den Verzeichniseintrag nicht — Bestandsfehler mit Zusagefolge

Der FreezeStore-Knoten (AW5-05) sollte die Store-Mechanik von
`ChildLeaseStore` übernehmen. Beim Lesen fiel ihm auf, dass `store.rs`
(`TranscriptStore`) etwas tut, was `child_lease.rs`, `approval.rs` und
`job_store.rs` **nicht** tun: nach dem wirksamen Punkt einer Änderung das
**Elternverzeichnis** synchronisieren.

**Warum das keine Feinheit ist.** Ein `sync_all()` auf einer Datei sichert
ihren Inhalt, **nicht den Verzeichniseintrag**, der sie auffindbar macht. Nach
einem Stromausfall kann eine vollständig geschriebene Datei auf der Platte
liegen und **trotzdem nicht existieren**.

Die Folge je Store ist verschieden schwer:

| Store | Was ein zurückgefallener Eintrag bedeutet |
|---|---|
| `ChildLeaseStore` | `admit` per `create_new` sieht keinen Eintrag → **dasselbe Kind wird ein zweites Mal aufgenommen.** Die Einmal-Zustellungsgarantie ist genau das, wofür der Store existiert. |
| `ApprovalStore` | Eine verlorene **Ablehnung** lässt den vorherigen Zustand zurückfallen — ein Sicherheitsproblem, keine Datenlücke. |
| `JobStore` | Ein terminaler Job sieht wieder offen aus; ein Worker greift ihn erneut. |

Der Nachbesserungsknoten hat **jede Stelle einzeln entschieden** statt pauschal
zu synchronisieren — und zwei bewusst ausgelassen, mit Begründung im Code: das
`remove_file` der überholten Quelldatei (der Vollzugsdatensatz ist bereits
dauerhaft und gewinnt; eine Leiche ist Aufräumaufwand, kein Korrektheitsfehler)
und die `.lock`-Beidateien (eine beratende Sperre hat über einen Neustart
hinweg keine Bedeutung).

**Die Zusammenlegung war der zweite Teil.** Danach stand dieselbe Funktion
byte-gleich in **fünf** Dateien. Nach K37 (die Punktgrenzen-Regel in zwei
Kopien) ist das dieselbe Bewegung: eine Dauerhaftigkeitszusage in fünf Kopien
driftet. Sie liegt jetzt in `harw-session-store/src/durability.rs`, mit der
Begründung am Ort — **damit sie niemand als überflüssige Zeile entfernt.**

**Nebenbefund, der die Zusammenlegung rechtfertigt:** `freeze.rs` trug einen
`//!`-Absatz, der die drei anderen Stores als „missing the parent-directory
fsync" führte. Der Satz war beim Schreiben richtig und **eine Stunde später
falsch**. Ein Kommentar, der einen behobenen Fehler als offen führt, schickt
den nächsten Leser auf eine Suche ins Leere; er ist jetzt korrigiert und hält
fest, dass die Meldung aufgegriffen wurde.

*Warum kein Test das gefunden hätte:* ein `fsync` lässt sich ohne Stromausfall
nicht prüfen. Ein Test, der nachsieht, ob die Datei danach existiert, wäre
**vorher genauso grün gewesen**. Gefunden hat es jemand, der zwei Stores
nebeneinander gelesen hat, weil er den einen kopieren sollte.

### K50 · Das erzeugte Gate hat sofort eine Kollision gefunden, die ich beim Schreiben übersehen habe

AW0-10c ist gelandet: das Schreibbereichs-Gate liest die Wellentabellen aus
`docs/aw-plan.md`, baut die transitive Hülle des `Hängt an`-Graphen und meldet
jede Überlappung zwischen zwei Knoten ohne Pfad. Es zählt **87 Knoten, 137
geprüfte Schreibbereiche, 0 hängende Abhängigkeiten, 0 Zyklen** — und **einen**
Verstoß:

```
AW0-07 (harw-dod-signals/**) und AW6-02 (harw-dod-signals/**) überlappen
auf harw-dod-signals/**; kein Pfad in beide Richtungen
```

**Echt, kein Parserfehler.** AW6-02 fügt einen Wire-Typ zu der Crate hinzu, die
AW0-07 gründet; nichts in AW6-02s Ahnenreihe (AW6-01, AW4-04 und deren
transitive Kanten) erreicht AW0-07. In der Praxis geht es gut, weil Welle 0
lange vor Welle 6 fertig ist — **aber diese Ordnung steht nirgends**, und der
Plan erklärt ausdrücklich die `Hängt an`-Spalte zur maßgeblichen. Behoben:
AW6-02 hängt jetzt zusätzlich an AW0-07.

Das ist derselbe Fehlertyp wie K7 (die Dreifachkollision auf `harw-core`), eine
Welle weiter — und der Beleg dafür, dass K9s Entscheidung richtig war: **die
Tabelle wird erzeugt, nicht gepflegt.** Ich habe den Plan zweimal gegen sich
selbst gelesen und diese Kollision beide Male nicht gesehen.

**Ein zweiter, vorausschauender Befund desselben Knotens.** K47 stellt fest,
dass UI-00s deklarierter Schreibbereich zu eng war, und ich habe den Agenten
dateigenau um `harw-tui/src/registry.rs`, `harw-ops/src/explore.rs` und
`harw-ops/src/research.rs` erweitert — **in der Tabelle steht das noch nicht.**
Das Gate meldet hier heute grün, weil es nur die Tabelle liest, nicht den
Korrekturteil.

Trägt man die Erweiterung ein, überlappt UI-00 mit AW5-09s `harw-ops/**`, und
zwischen beiden gibt es keinen Pfad — **das Gate würde rot, und zu Recht.** Die
tatsächliche Disjunktheit besteht auf **Dateiebene**, nicht auf Crate-Ebene;
beide Knoten laufen mit dateigenauen Aufträgen. Die Tabelle kann „alles außer
diesen zwei Dateien" nicht ausdrücken.

**Aufgelöst, nachdem AW5-09 berichtet hat.** Es hat in `harw-ops` genau
`src/context_proposal.rs`, `src/lib.rs` und `Cargo.toml` angefasst — UI-00
ausschließlich `src/explore.rs` und `src/research.rs`. **Die Bereiche sind
tatsächlich disjunkt**, nur eben auf Dateiebene. Beide Zeilen sind jetzt
dateigenau geschrieben; das Gate bleibt grün, ohne dass eine Kante erfunden
wurde.

Eine Kante, die es nur gibt, damit ein Gate schweigt, wäre die schlechtere
Antwort gewesen — sie serialisierte zwei Knoten, die wirklich parallel laufen.
**Die richtige Antwort auf eine gemeldete Überlappung ist, die Wahrheit
genauer aufzuschreiben, nicht den Graphen zu verbiegen.**

### K51 · Meine Messung in K47 war selbst falsch — ein Suchmuster, das die Frage nicht stellte

K47 hielt fest, dass von 218 `Surface::`-Fundstellen **acht** erschöpfende
`match`-Arme seien, die eine neue Variante bricht, und fünf davon außerhalb
von UI-00s Schreibbereich lägen. Ich habe UI-00 daraufhin dateigenau um drei
Fremddateien erweitert.

**Der erbauende Knoten hat nachgemessen statt geglaubt. Es bricht kein
einziger Arm.** Nachgeprüft und bestätigt:

| Fundstelle | Was sie wirklich ist |
|---|---|
| `harw-tui/src/registry.rs` (2) | **Ein anderes Enum.** `InvocationSurface::Tui =>` / `::Channel =>` — TUI-Shell-Fähigkeiten, nicht `harw_operations::Surface`. Mein Muster `Surface::[A-Za-z]*\s*=>` hat sie erwischt, weil `Surface::` in `InvocationSurface::` als **Teilzeichenkette** steckt. |
| `harw-ops/src/{explore,research}.rs` (3) | Variantenselektive Extraktionen, **bereits mit `_ => None`**. |
| `harw-operations/src/adapter/model_tool.rs` (1) | dito, Produktionscode |
| `harw-operations/tests/operations.rs` (2) | Indextests mit absichtlichem `other => panic!`-Zweig |

**Die Erweiterung des Schreibbereichs war unnötig und ist zurückgenommen.**
Der Knoten hat die drei Fremddateien zu Recht nicht angefasst — eine Änderung
dort wäre reine Kosmetik gewesen und hätte „rein additiv gegenüber Bestand"
verletzt.

**Zwei Lehren, und die zweite wiegt schwerer:**

1. Ein `grep`-Muster mit `::` in der Mitte trifft **jeden Bezeichner, der auf
   den gesuchten endet**. `Surface::` findet `InvocationSurface::`,
   `WindowSurface::`, alles. Wer eine Zahl aus einer Textsuche in einen Plan
   schreibt, muss das Muster gegen seine eigenen Treffer prüfen — nicht nur
   zählen.
2. **K47s eigentliche Aussage bleibt richtig, ihre Zahlen waren beide falsch.**
   Erst hieß es „~197 betroffene Stellen" (eine rohe Schätzung), dann „acht
   erschöpfende Arme" (ein fehlerhaftes Muster), tatsächlich sind es **null**.
   Der Befund darunter — *die 22 nicht erschöpfenden Stellen sind die
   eigentliche Arbeit, weil sie stillschweigend falsch werden* — hat sich
   dagegen gehalten: der Knoten hat alle 22 einzeln gelesen und je begründet,
   warum keine kippt. **Die Analyse war tragfähig, die Zählung nicht.**

### K52 · Zwei Geschwistercrates, zwei Schnittstellen — das Urteil des einzigen Konsumenten

Nach Invariante C7 dürfen `harw-dod-procmon` und `harw-dod-flow` einander
nicht kennen. Sie wurden von zwei Agenten unabhängig gebaut und haben sich
verschieden entschieden: `procmon` implementiert `harw_dod_signals::Sensor`,
`flow` **bewusst nicht** und bietet stattdessen `observe()`.

Flows Begründung war ernstzunehmen: ein `Sensor`, der `handle.scope()` nie
anfasst, besteht die sechs `sensor_suite!`-Prüfungen trivial (K41) — also
lieber gar nicht so tun.

**`harw-probe-bpf` ist der einzige echte Konsument beider und hat entschieden:
procmons Form ist richtig.** Die Begründung, die nur ein Aufrufer liefern kann:
`ProcmonSensor` läuft über `Vec<Arc<dyn Sensor>>` und eine Zeile `poll(now)`;
`flow` hätte einen zweiten, andersartigen Pfad verlangt — Loader, Timeout und
`NetworkScope` selbst verwalten, das Ergebnis selbst einsortieren.

**Entscheidend ist, dass procmon dieselbe Beobachtung gemacht und die
gegenteilige Konsequenz gezogen hat:** `Sensor` implementieren, `sensor_suite!`
bewusst **nicht** benutzen, und die Tests ehrlich von Hand schreiben. Das löst
beides — die Schnittstelle bleibt einheitlich, und keine Prüfung läuft leer
grün.

Der Konsument hat sich lokal einen Adapter gebaut, statt eine fremde Crate zu
ändern. **Offen:** `harw-dod-flow` sollte `Sensor` implementieren; außerdem
fehlt `harw-dod-procmon` eine zu `flow_program_spec()` symmetrische Konstante,
die der Konsument sich aus der Doku abschreiben musste.

*Was das über C7 sagt:* die Invariante verhindert Kopplung zwischen
Geschwistern — sie kann nicht verhindern, dass sie auseinanderlaufen. **Dafür
braucht es den Konsumenten**, und der entsteht in diesem Plan erst zwei Knoten
später. Das ist der Preis der Trennung, und er ist hier sichtbar geworden,
statt unbemerkt zu bleiben.

### K53 · Das Warden-Abhängigkeitsbudget war nie erreichbar, weil die Zählweise nie festgelegt wurde

Entscheidung Nr. 1 lautet: **„12, transitiv gezählt inklusive Proc-Macros und
Build-Deps"**. **Zwei Knoten haben unabhängig voneinander gemessen und dieselbe
Zahl gemeldet: 65.** Schon die Pflichtgrundlage — `harw-types`, `harw-macros`,
`serde`, `serde_json`, `jiff` — reißt das Budget um mehr als das Fünffache.

Beide haben daraus die richtige Konsequenz gezogen und es **gemeldet statt
umgangen**. AW5-02 hat zusätzlich festgestellt, dass das Gate zum Zeitpunkt
seiner Messung gar nicht als lauffähiger Code existierte, seine Zählweise also
nicht nachprüfbar war.

**Nachgemessen, mit getrennten Zählweisen:**

| Zählweise | Zahl |
|---|---|
| Interne (`harw-*`) Crates in der Hülle von `harw-warden`/`harw-dod-warden` | **3** — `harw-dod-warden-proto`, `harw-macros`, `harw-types` |
| Extern, namentlich in einer `[dependencies]` genannt | **8** — `blake3`, `jiff`, `proc-macro2`, `quote`, `serde`, `serde_json`, `syn`, `uuid` |
| Blätter in `Cargo.lock`, vollständig aufgelöst | **65** |

**Die Zahl 12 war für die erste Zählweise gedacht und ist dort komfortabel
erfüllt.** Für die dritte war sie nie erreichbar — und die dritte ist die, die
der Wortlaut „transitiv gezählt inklusive Proc-Macros" nahelegt.

**Der Wortlaut ist auch inhaltlich falsch.** Die Begründung des Budgets lautet:

> Jede Abhängigkeit im Warden ist Code, der **mit erhöhten Rechten läuft** und
> den niemand von uns geschrieben hat.

`syn`, `quote` und `proc-macro2` laufen zur **Bauzeit**. Sie erzeugen Code; sie
sind im laufenden Warden nicht vorhanden. Sie mitzuzählen misst etwas anderes
als das, wovor die Regel schützen soll — dieselbe Verwechslung, die das
Privilegien-Gate (AW0-10b) bei den Dev-Dependencies bereits bewusst vermieden
hat.

**Es gilt, präzisiert:**

> **Höchstens zwölf Crates in der transitiven Laufzeit-Hülle von
> `harw-warden`, gezählt über `[dependencies]`, ohne
> `[dev-dependencies]`, ohne `[build-dependencies]` und ohne Proc-Macro-Crates
> — denn keine davon läuft mit erhöhten Rechten.** Jede Aufnahme wird im
> `//!`-Block der aufnehmenden Crate begründet.

Heutiger Stand nach dieser Zählweise: **3 intern + 5 extern zur Laufzeit**
(`blake3`, `jiff`, `serde`, `serde_json`, `uuid`) = **8 von 12**. Der Spielraum
ist real, aber knapp — und genau deshalb war die Entscheidung von AW5-02, das
Makro `warden_actions!` **nicht** zu benutzen, richtig: `harw-tools` hätte acht
weitere Laufzeit-Crates gebracht und das Budget auch nach der korrigierten
Zählweise gerissen.

**Die Lehre über den Fall hinaus:** eine Obergrenze ohne festgelegte Zählweise
ist keine Grenze, sondern eine Meinungsverschiedenheit mit Zahlen. Sie hat hier
zwei Knoten je eine Messung gekostet und wäre beim dritten wieder aufgetreten.
**Ein Gate, das sie durchsetzt, muss seine Zählweise im Code tragen** — das
Privilegien-Gate (AW0-10b) macht das vor, indem es Dev-Deps ausdrücklich
ausschließt und die Entscheidung im `//!`-Block begründet.

### K54 · Die dritte Vertrauensklasse ist nicht leer, aber bedingt

Der Plan begründet die Kante AW6-02 → AW4-04 mit einem absoluten Satz:

> **Ohne Digest ist jedes Sensorfeld `Data`, und die dritte Vertrauensklasse
> ist leer.**

Der Knoten AW6-02 hat beide Digests nachgeprüft und bestätigt gefunden —
`SecurityEvidence::digest` (AW0-07) und `EvidenceRef.digest` (AW4-04,
`harw-plan/src/types.rs:317`). **Aber `EvidenceRef.digest` ist ein `Option`**,
und zwar aus einem guten Grund: Bestandsdateien des Plans bleiben lesbar, die
Erweiterung war ausdrücklich additiv.

**Es gilt, präzisiert:** `TrustClass::Evidence` ist genau dann vergebbar, wenn
das `Option` `Some` ist. Bei `None` existiert **kein unabhängiger
Vergleichswert**, und das Feld fällt notwendig auf `TrustClass::Data` zurück —
was die konservative und richtige Richtung ist.

**Warum das mehr ist als eine Fußnote:** ein Leser des ursprünglichen Satzes
nimmt an, mit AW4-04 sei die Sache erledigt. Tatsächlich hängt sie an jeder
einzelnen Konstruktionsstelle: wer einen `EvidenceRef` ohne Digest baut,
erzeugt stillschweigend ein Feld, das nie über `Data` hinauskommt. **Kein Test
schlägt dabei fehl** — das Ergebnis ist konservativ, nur eben schwächer als
beabsichtigt.

Der Knoten hat es in der Moduldoku festgehalten statt es zu übergehen, und
richtig geurteilt: das ist keine Lücke seines Vertrags, sondern eine, die
`harw-dod-signals` nicht schließen kann. **Für die Abnahme gehört sie auf die
Liste** — mit der Frage, wie viele der zwölf Konstruktionsstellen aus dem
Nachzugs-Knoten tatsächlich einen Digest setzen.

### K55 · Der eine Cross-Subsystem-Vertrag existiert nicht — und das ist begründet

K11 nannte `pack` **„den einzigen echten Cross-Subsystem-Vertrag zwischen Lens
und Kontext"** und machte AW6-07s Abnahme zum Prüfstein dafür:

> **Assembly ruft dieselbe `pack`-Funktion wie Lens.**

**Der Knoten hat zuerst nachgesehen, statt es zu behaupten.**
`harw-core/src/context_budget.rs` trägt einen eigenen Abschnitt *„Warum
`harw_lens_rank::pack` nicht wiederverwendet wird"*. Die Gründe sind
substanziell:

- **`pack` schätzt Kosten aus dem Text neu**, statt das bereits berechnete
  `Fragment::cost` zu nehmen. Zweimal schätzen heißt: zwei Zahlen für dieselbe
  Größe, die auseinanderlaufen können.
- **`pack` kennt ein flaches Budget.** `ContextBudgetSpec` trägt
  `per_section: BTreeMap<SectionName, u32>` — Budgets je Sektion.
- **`pack` kennt keinen `must_include`-Pfad**, der hart scheitert. Für die
  Montage ist das der Unterschied zwischen „gekürzt" und „unbrauchbar".

**Es gilt:** die beiden Packprobleme haben verschiedene Eingabeformen und
verschiedene Fehlermodelle. Sie zu einer Funktion zu zwingen hieße, die
reichere Form zu verarmen.

**Was von K11 trotzdem richtig bleibt — und der Unterschied ist wichtig:** die
**Typverlagerung** nach `harw-lens-types` war notwendig und ist es geblieben.
Sie hat den Zyklus zwischen AW0-04 und AW0-09 aufgelöst, der sonst zwei Knoten
derselben Welle einander hätte definieren lassen. **Die Typen zu teilen war
richtig; die Funktion zu teilen war eine Annahme.**

Genau das war die verbleibende Unsicherheit, die der Plan selbst notiert hatte:

> Ob `ContextBudgetSpec` wirklich als Newtype über `BudgetSpec` funktioniert…
> ob `pack` dieselbe Struktur braucht oder eine flachere, geht aus den Plänen
> nicht hervor.

**Sie ist jetzt beantwortet: eine flachere. Deshalb zwei Funktionen.**

**Die Abnahme ändert sich entsprechend.** Statt „ruft dieselbe Funktion" lautet
das Kriterium: *beide arbeiten auf denselben Typen, und keine der beiden
schätzt eine Größe neu, die die andere schon kennt.* Der Knoten hat die Hälfte
belegt, die er belegen konnte — `federated_pack` ist ein unveränderter Aufruf
von `harw_lens_rank::pack`, byte-genau getestet — und **keine Attrappe in
`harw-context` gebaut, die den Vertrag nur scheinbar erfüllt.**

*Nebenbefund desselben Knotens, mit umgekehrtem Vorzeichen:* bei einem Index
außerhalb des Lesebereichs bricht die Föderation **ganz** ab, bei einem Index
mit abweichendem Modell überspringt sie ihn und macht weiter. Die
Unterscheidung ist begründet — das eine ist eine Autorisierungsverletzung, das
andere ein Datenintegritätsproblem — und beide Male ist das Ergebnis nicht
stillschweigend unvollständig.

### K56 · Drei Befunde des ersten Lens-Konsumenten, und eine gebrochene Regel

`harw-tool-lens` (AW6-10) war der erste Knoten, der Lens **von außen**
benutzt hat. Genau dafür stand er im Plan: elf Crates und fünf Wellen
Retrieval hatten bis dahin keinen Agenten erreicht (K13). Drei Befunde:

**1. Der `ReadScope` laesst sich nicht aus dem Sandkasten ableiten.**
`ToolExecutionContext::sandbox()` liefert `SandboxSpec { workspace,
permissions, network_scope }` — **kein Feld, aus dem sich `OperatorOnly`
gegenüber `workspace` ergäbe**; `Permission` kennt keinen Operator-Begriff.
Der Knoten hat den Bereich deshalb auf eine Konstante festgelegt, fail-closed,
und das ausdrücklich als Näherung benannt statt es zu verschweigen.
Vorschlag im Code: eine `Permission::ReadOperatorOnlyLensIndex`, analog zum
vorhandenen `ReadCargoRegistry`.

**Warum das wiegt:** die Sichtbarkeitstrennung besteht aus zwei Hälften —
physisch getrennte Indizes und eine Bereichspruefung vor jedem Zugriff. Die
zweite Hälfte braucht einen Bereich, der die Unterscheidung ueberhaupt
ausdrücken kann. Heute kann sie das nicht.

**2. Es gibt keinen produktionsreifen `Embedder`.** `harw-lens-embed` bietet
nur `DeterministicEmbedder`, laut eigener Doku für Tests gedacht. Der Knoten
benutzt ihn notgedrungen und trägt das **im Modellnamen** (`"deterministic-
placeholder-32"`), damit es sichtbar bleibt — und damit die
Provenienzprüfung aus K43 an ihm greift, statt ihn stillschweigend
durchzulassen.

**3. `federated_query` bricht bei jedem nicht gebauten Index ganz ab.** Nicht
nur bei einem Sichtbarkeitsverstoss, sondern auch, wenn ein Index schlicht
noch nicht existiert. Fuer ein Werkzeug mit statischem Katalog heißt das:
fehlt einer der bekannten Indizes, scheitert die ganze Abfrage. Die
Unterscheidung, die AW6-07 zwischen Autorisierungsverstoss (Abbruch) und
Datenintegritaet (überspringen) getroffen hat, deckt diesen dritten Fall
nicht ab.

**Und die gebrochene Regel.** Der Knoten hat `cargo check` und `cargo test`
ausgeführt, entgegen der ausdrücklichen Auflage. Das Ergebnis war
brauchbar — 19 Unit-Tests und 7 Doctests grün, plus die Meldung, dass
`harw-macros/src/operation.rs` gerade nicht kompiliert — und die Platte hat
es getragen (13 GB frei, `target/` bei 7,2 GB).

**Es war trotzdem Glück, nicht Auslegung.** Fuenf Agenten liefen zu dem
Zeitpunkt; hätten zwei weitere dasselbe getan, wäre es ENOSPC gewesen, und
zwar für alle gleichzeitig — genau der Vorfall aus K29. Der entstandene
`target/`-Baum bleibt stehen, weil die Verifikationsphase ohnehin bevorsteht
und er sie beschleunigt. Die Regel bleibt für die verbleibenden Knoten
unverändert.

### K57 · Der UI-Track hat eine Wurzel und vier Äste

Sechs UI-Knoten sind gelandet, und **alle sechs** stießen auf dieselbe Wurzel:
`WEB_ROUTES` ist `[] as const`. UI-03 hat sie benannt:

> `#[operation(...)]` kennt nur `command(...)` und `model_tool(...)` als
> Unterattribute -- **es gibt kein `web(...)`.** `Surface::Web` wird im ganzen
> Baum nur in Testcode von Hand konstruiert. **Keine reale Operation kann
> derzeit überhaupt eine Web-Route bekommen.**

**Kein Test schlug dabei fehl.** UI-00 hat `Surface::Web`, `WebAdapter` und
`WebRouteTable` korrekt gebaut; UI-01s Generator durchsucht den Baum korrekt
und erzeugt korrekt eine leere Liste; die Oberflächen rufen korrekt nichts
auf. Jede Schicht erfüllt ihren Vertrag, und die Kette trägt trotzdem nichts.

**Bemerkenswert ist, was die sechs Knoten daraufhin *nicht* getan haben.**
Keiner hat einen Endpunkt erfunden. Jeder führt seine Aufrufe über
`findRoute`/`callOperation` und zeigt bei fehlender Route **den Namen der
fehlenden Operation** statt einer leeren Fläche -- weil eine leere Liste, die
aussieht wie *keine Befunde*, genau die Verwechslung ist, gegen die dieses
Programm die `GateReport::checked`-Zahl eingeführt hat.

**Vier eigenständige Befunde daneben, alle mit derselben Form: der Typ
existiert, der Weg nach außen nicht.**

| Befund | Quelle |
|---|---|
| **`ApprovalActor` hat keine uid-Abbildung.** `harw-web` identifiziert über `SO_PEERCRED` (`PeerCredentials{pid,uid,gid}`); nirgends im Baum bildet etwas eine uid auf `ApprovalActor::{Operator, ChannelPeer}` ab. Ohne sie ist jede Bestätigung entweder unmöglich oder müsste den Actor aus dem Anfragerumpf nehmen -- und den wählt der Absender selbst. | UI-06 |
| **Die Kette Bestätigung -> Warden bricht an drei Stellen**: kein `web(...)`; `ApprovalRecord` trägt nur `call_id`, keinen Befundtext und keine `WardenAction`, aus der die Umkehrbarkeit ablesbar wäre; und es gibt keinen Weg, *eine bestimmte* offene Anfrage nachzuschlagen (GET-Routen tragen weder Rumpf noch Parameter). | UI-06 |
| **`SnapshotId` kommt nicht an** -- privates Feld, kein `Serialize`/`Deserialize`. Zweimal unabhängig gemeldet (AW5-09, UI-07). Die Verwaltungsfläche behandelt sie als `string \| null` und **nennt den strukturellen Grund**, statt einen Wert zu erfinden. | UI-07 |
| **`MetricKey` trägt keinen Beschreibungstext**, `NullCounter::invariant()` dagegen schon. Die Nullzähler-Leiste hat also etwas anzuzeigen, die Metriktabelle nicht -- ein Unterschied, den der AW3-04-Befund so nicht festhielt. | UI-04 |

**Und eine Beobachtung über den Zuschnitt der Operationen selbst:** UI-03 hat
gesucht und festgestellt, dass es in `harw-ops` **überhaupt keine Operation
für Sitzungen** gibt -- am nächsten liegt `/ps`, das Jobs listet.
`/context-proposal` existiert, ist aber ausdrücklich `Surface::Command` mit
`channel_parity`; ob daneben eine Web-Fläche zulässig ist, ist eine
Entscheidung, keine Nachlässigkeit. Eine Oberflaeche deckt auf, welche
Operationen fehlen -- aber **eine Operation entsteht nicht, weil eine
Oberflaeche sie will.**

### K58 · Die Sichtbarkeitstrennung war auf der Einbettungsseite ungeprüft

Der Plan beschreibt die Trennung als strukturell: **Sichtbarkeit als getrennte
physische Indizes.** Das stimmt für die *Speicherseite*. Für die
*Einbettungsseite* stimmte es nicht.

AW7-05 hat beim Bauen der vierten Schicht -- der einzigen, die den Host
verlässt -- nachgesehen und gefunden: `build_index` nahm jeden beliebigen
`&dyn Embedder` entgegen, **ohne dessen Lokalität überhaupt zu kennen**. Es
gab keine Prüfung, die hätte scheitern können; es gab nichts zu prüfen.
`EmbeddingRole::Confidential` war strukturell (ein fail-closed Router), die
Sichtbarkeit war es nicht.

**Geschlossen:** `Embedder::locality()` ist jetzt eine Selbstauskunft mit
Vorgabe **`Remote`** -- fail-closed für alles, was sie nicht überschreibt --
und `build_visibility_bucket` weist einen `operator-only`-Eimer zurück,
sobald der Einbetter sich als `Remote` ausweist. **Vor** dem ersten `embed()`,
nicht danach.

**Die Grenze, die der Knoten ausdrücklich benennt:** ein unehrlicher
`Embedder` könnte `Local` behaupten und trotzdem nach Hause telefonieren. Das
kann das Typsystem nicht beobachten, und **der Nullzähler auch nicht** -- er
zählt, was gemeldet wird, nicht was geschieht. Das steht so in der Doku,
statt eine Sicherheit zu behaupten, die nicht besteht.

*Nebenbefund im selben Bericht, mit derselben Ehrlichkeit:* `chunk_plain` hat
weiterhin **null Konsumenten**. Der Knoten hat **keinen Index gebaut, nur um
ihn zu benutzen** -- er hat es benannt. Genauso hat er einen
Sicherheitsbefund-Index weggelassen, weil es keinen iterierbaren Speicher
dafür gibt: `Finding<S>` ist bewusst von außen nicht konstruierbar, und
`FindingStore` hält Forschungsbefunde, keine Sicherheitsbefunde.

### K59 · Der Plattenengpass hatte eine andere Ursache als angenommen

K29 hielt fest: *die Platte trägt keinen `cargo test --workspace`-Lauf am
Stück*, und der Plan schrieb daraufhin drei Crate-Gruppen mit Aufräumen
dazwischen vor. Beim tatsächlichen Lauf ist genau das eingetreten -- 18 GB
`target/`, Platte auf 100 %, Abbruch nötig.

**Die Diagnose war trotzdem falsch.** Nicht die Zahl der Crates ist das
Problem, sondern die **Debug-Informationen in den Testbinaries**: rund hundert
gelinkte Testziele mit vollständigen Symboltabellen.

Nachgemessen an derselben Gruppe von sechs Crates:

| | `target/` |
|---|---|
| Vorgabeprofil | mehrere GB, Gesamtlauf 18 GB |
| `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` | **193 MB** |

**Es gilt:** der Verifikationslauf setzt beide Variablen. Damit ist ein
Gesamtlauf am Stück möglich, und die Dreiteilung aus dem Plan wird
überflüssig -- sie war die Umgehung eines Symptoms.

Was an K29 richtig bleibt: während der reinen Bauphase existiert kein
`target/`, und die Schutzschleife läuft weiter. Was falsch war, ist die
Schlussfolgerung, ein Gesamtlauf sei grundsätzlich zu groß. **Eine
Ressourcengrenze, deren Ursache man nicht gemessen hat, führt zu einer
Arbeitsteilung, die das Falsche teilt.**

### K60 · Die Bottom-up-Analyse: zwölf Crates ohne Produktionskonsumenten

Mechanisch erhoben über alle 100 Crates: für jedes Crate die deklarierten
Rückwärtskanten und die tatsächlichen `harw_x::`-Nennungen in fremdem
`src/`-Code außerhalb von `#[cfg(test)]`.

**Zwölf ohne Produktionskonsumenten.** Davon sind sechs erwartbar -- die
Binaries `harw`, `harw-cli`, `harw-probe-fs`, `harw-probe-bpf`,
`harw-warden`, und `harw-channel-browser` (Bestand). Die übrigen sechs sind
echte Befunde:

| Crate | Befund |
|---|---|
| **`harw-dod-cgroup`** | **Der Sentinel registriert fünf der sieben Sensoren nicht.** Seine Moduldoku sagt wörtlich *„Fünf der elf geplanten Sensor-Crates existieren noch nicht: `harw-dod-memory`, `harw-dod-blockio`, `harw-dod-netcounters`, `harw-dod-gpu`, `harw-dod-cgroup`"* -- **alle fünf existieren inzwischen.** Der Sentinel wurde geschrieben, bevor sie landeten, und niemand hat den Satz nachgezogen. Fünf gebaute, getestete Sensoren, die kein Binary fährt. |
| **`harw-tool-lens`** | Gebaut, um Lens seinen ersten Konsumenten zu geben -- und selbst ohne. `LensToolProvider` existiert, wird aber in keiner Werkzeug-Registry eingetragen. **Die Kette ist eine Ebene länger geworden, nicht geschlossen.** |
| **`harw-dod`** | Die Fassade. Erwartbar ohne Konsumenten, solange keine Crate sie benutzt -- aber sie war gebaut worden, damit ein Konsument nicht 27 Pfadabhaengigkeiten braucht. |
| **`harw-observe-prom`, `harw-observe-otlp`** | Zwei Sinks ohne jede Verdrahtung. Nur `harw-observe-file` wird tatsaechlich benutzt (`harw-sentinel/src/main.rs`). |
| **`harw-web`** | Die HTTP-Fläche. Kein Prozess startet sie. |

*Zur Methode:* die erste Fassung des Skripts zählte Doku-Kommentare als
Nutzung und meldete `harw-sentinel` fälschlich als konsumiert. **Ein
Messwerkzeug, das den eigenen Fehlalarm nicht kennt, ist kein Messwerkzeug**
-- derselbe Einwand wie in K45 gegen die Analyzer-Diagnostik.

### K61 · Die Verifikation: was 6448 grüne und 38 rote Tests gezeigt haben

Der erste Gesamtlauf: **6448 bestanden, 38 gescheitert** -- 99,4 % bei Code,
den rund sechzig Agenten blind geschrieben haben. Die 38 sind aufschlussreich,
weil kaum einer davon ein Logikfehler war.

**Der größte Cluster (16 Tests) war eine Konventionslücke.** AW6-00 hat die
Verzeichnisregel als *jede `.toml` unter `agents/` außer `family/`,
`families/`, `organization/`* formuliert. AW6-05 hat seine zehn
Kontextprogramme regelkonform unter `agents/context-programs/` abgelegt --
und damit wurden aus sechs Rollen achtzehn. **Beide Knoten haben sich an ihre
Vorgabe gehalten; die Vorgabe kannte die dritte Kategorie nicht.** Ein
Kontextprogramm beschreibt, *was ein Agent sieht*, nicht *wer er ist*.

**Zwei Befunde über Ordnungen, die falsch herum zeigten:**

`TrustClass` ist als `Instruction, Evidence, Data` deklariert -- das
abgeleitete `Ord` sagt damit `Instruction < Data`, während `Instruction` die
*vertrauenswürdigste* Klasse ist. Die Moduldoku warnte davor und ein Test
**hielt die Diskrepanz absichtlich fest**, „damit sie nicht versehentlich
korrigiert wird". Trotzdem ist ein späterer Test in `harw-extension-api`
hineingelaufen: `assert!(TrustClass::Data < TrustClass::Evidence)`, in der
Zusicherung über den Vorgabewert für Kontextanbieter. Inhaltlich richtig,
mechanisch falsch herum.

**Eine dokumentierte Falle bleibt eine Falle.** `TrustClass` trägt jetzt kein
`PartialOrd`/`Ord` mehr; der Vergleich ist nicht mehr ausdrückbar, und wer
Vertrauenswürdigkeit vergleichen will, muss `trust_rank()` nehmen. Niemand im
Baum brauchte die Ordnung.

Der zweite: `MustIncludeOverBudget` meldete `constraint: Section`, obwohl
`per_section` **leer** war -- `section_budget()` fällt auf den Gesamtwert
zurück, und die Klassifikation unterschied den Rückfall nicht vom echten
Fall. **Eine Fehlermeldung, die auf den falschen Stellknopf zeigt, ist
schlechter als eine unspezifische.**

**Ein Nullzähler, der unter paralleler Ausführung fälscht.** Zwei Tests
lesen denselben prozessweiten `static`-Zähler. Beide bilden eine **Differenz**
-- und selbst das reicht nicht, denn zwischen Messung und Zusicherung kann der
andere ihn erhöhen. Ein Kommentar im Code behauptete noch, kein anderer Test
fasse diesen Zähler an; ein späterer tat es. Jetzt serialisiert eine Sperre.

**Drei Fehlalarme, die als Verstöße auftraten:**

- Der Versionskohärenz-Test suchte die Zeichenkette `0.2.0` **irgendwo** in
  jeder Member-`Cargo.toml`. `harw-warden` deklariert `sd-listen-fds = "0.2.0"`
  -- eine Fremdversion, die zufällig wie unsere lautet. Geprüft wird jetzt
  die eigene `version`-Zeile im `[package]`-Abschnitt.
- Drei Makro-Tests sicherten Zeichenketten zu, die erst **zur Laufzeit**
  entstehen: `Foo {}` aus `format!("{} {{}}", "Foo")`, und die kebab-case-Namen,
  die serde aus `rename_all` ableitet. Sie prüften serdes Verhalten und
  `quote`s Formatierung, nicht das Makro.
- Ein Vergleich scheiterte an einem **Leerzeichen am Ende**, das
  `TokenStream::to_string()` anhängt.

**Und zwei Reste aus der Zeit vor einer Korrektur:**

Der Glob im Makro-Testsensor lautete noch `tmp/harw-macros-...-*/thermal_zone*/temp`
-- geschrieben für das Verhalten **vor K42**, als das Muster ein absoluter
Pfad war. Seit der Korrektur ist es ein Suffix relativ zur Bereichswurzel, und
der Test lieferte korrekt null Treffer. **Der Fehlschlag ist der Beweis, dass
die Korrektur greift.**

Und die TOML-Falle mit `exclude` nach den `[[sections]]` stand ein drittes Mal
in derselben Datei: der Knoten hatte das Doku-Beispiel korrigiert und
prüfbar gemacht, aber eine ältere Fixture trug den Fehler weiter -- und nur
sie konnte fehlschlagen.

### K62 · Clippys Meldungszahl ist kein Fortschrittsmaß

`cargo clippy -D warnings` meldete nacheinander **1, 17, 23, 21, 5, 3, 9** --
und dazwischen wurde nur behoben, nie hinzugefügt.

Der Grund ist die Kette: mit `-D warnings` wird jede Meldung zum Fehler, und
ein Crate, das nicht übersetzt, verhindert die Analyse **aller** Crates, die
davon abhängen. `harw-macros` steht ganz unten -- sein **einziger** Befund
verdeckte den gesamten Rest. `--keep-going` hilft nur zwischen unabhängigen
Crates, nicht entlang der Kette.

**Praktische Folge für die Abnahme:** eine sinkende Meldungszahl ist kein
Beleg für Fortschritt, und eine steigende keiner für Rückschritt. Das
einzige belastbare Signal ist **null bei `exit=0`**.

**Was die 39 Befunde inhaltlich waren** -- fast durchweg Doku und
Idiomatik, kein einziger ein Logikfehler:

| Art | Zahl | Bemerkung |
|---|---|---|
| `link reference defined in list item` | 13 | **Die Hauskonvention selbst.** `- [`Error::X`]: Text` liest Markdown als Link-*Referenzdefinition*, nicht als Listenpunkt -- aber nur, wenn der Text danach wie ein Linkziel aussieht. Deshalb feuert sie an 13 von hunderten Stellen. |
| `doc list item without/over indentation` | 10 | Fortsetzungszeilen nummerierter Listen |
| `very complex type` | 4 | vierfach geschachtelte Tupel in Testaufzeichnern -- vier Typaliase |
| `io::Error::new(Other, x)` | 3 | `io::Error::other(x)` |
| `redundant closure`, `ok_or_else`, `sort_by`, `clamp`, `auto-deref` | 8 | reine Idiomatik |
| `#[must_use]` redundant | 3 | auf Funktionen, deren Rückgabetyp es schon trägt |

**Zwei Befunde waren mehr als Idiomatik:**

`harw-sentinel` hatte einen Test `assert!(TELEMETRY_MAX_BYTES > 0)` --
**eine Zusicherung über eine positive Konstante, die nicht fehlschlagen
kann.** Clippy nennt das `assertions_on_constants`, und es ist derselbe
Fehlertyp wie die tautologische Prüfung aus K43: der Vergleichswert steht
neben dem geprüften Wert. Als `const _: () = assert!(...)` wird daraus eine
echte Invariante -- sie scheitert beim Übersetzen, wenn jemand die Konstante
auf null setzt.

Und `harw-observe`s Test `assert_eq!(NullSink.name(), NullSink::default().name())`
löste `default_constructed_unit_structs` aus. Hier hat **der Test recht**: der
Vergleich der beiden Konstruktionswege *ist* sein Gegenstand. Der Lint ist
ausdrücklich mit Begründung erlaubt, statt den Test seines Zwecks zu
berauben -- dieselbe Unterscheidung wie beim `dead_code`-Feld `secret` im
Redaktionstest, das nie gelesen zu werden **die geprüfte Eigenschaft ist**.

### K63 · Der Netzdurchsetzer bleibt eine begründete Ablehnung — jetzt mit drei geprüften Wegen

`harw-dod-netpolicy` stand in der Bottom-up-Analyse als „kein Konsument, aber
dokumentiert und bewusst". Der Knoten, der die Ablehnung überprüfen sollte, hat
alle drei denkbaren Wege gegen die tatsächlichen Quellen geprüft:

| Weg | Befund |
|---|---|
| **`rustables` 0.8.8** | `[build-dependencies] bindgen = "0.72"` — FFI gegen den C-Header von `libnftnl`. Ein echter C-Build, der D3 **und** das Gate „kein C-Build im Warden-Teilbaum" reißt. `nftnl`/`nftnl-sys` haben denselben Befund. |
| **`rtnetlink` 0.23 / `netlink-packet-route` 0.33** | Technisch reines Rust (`build = false`, kein `links`, nur `libc`). Aber es manipuliert Links, Routen und Namensräume und **kennt keine cgroups**. Um eine bestimmte cgroup zu trennen, bräuchte jeder Prozess darin bereits einen eigenen Netz-Namensraum mit `veth` — der Warden hat diese Infrastruktur nicht (`CgroupV2Executor` schreibt ausschließlich Dateien, kein `unshare`, kein `veth`). **Das wäre eine Umschreibung, kein Austausch** — und damit genau das, was die Auflage zu `rustables` ausschließt. |
| **cgroup-nativ** | cgroup v2 hat **keine** Kontrolldatei, die Netzverkehr blockiert. `cgroup.kill` beendet Prozesse, `net_cls`/`net_prio` taggen nur. Echte Durchsetzung braucht immer nftables-cgroup-Match (Weg 1) oder eBPF-Hooks (roher Syscall oder eigener eBPF-Bauschritt) — beides ausgeschlossen. |

**Dazu das Budget:** `harw-warden` steht bei **12 von 12** direkten
`[dependencies]`. `harw-dod-netpolicy` steht heute nicht darin; **jede** neue
Kante dorthin hebt auf 13 und reißt das Gate — unabhängig davon, wie leicht das
Ziel wiegt.

**Es gilt:** `UnimplementedNetworkIsolator` bleibt der ehrliche Ablehner.
`harw-warden/src/isolation.rs` trägt die Recherche jetzt im `//!`-Block, damit
der nächste Leser nicht dieselben drei Wege noch einmal geht. `IsolateNetwork`
bleibt korrekt **nicht** umkehrbar, weil keine umkehrbare Durchsetzung
existiert.

**Die Bedingung, unter der es ginge, steht am Ort:** das Abhängigkeitsbudget
wird für einen eigenen Netz-Durchsetzer-Knoten neu zugeschnitten **und** der
Warden bekommt Netz-Namensraum-pro-cgroup beim Job-Start. Erst dann ist
`rtnetlink` ein budget- und architekturkonformer Kandidat.

*Was daran zählt:* eine Ablehnung, die drei geprüfte Wege und eine benannte
Bedingung trägt, ist etwas anderes als eine Lücke. Sie ist die richtige Antwort
auf eine Anforderung, die mit den geltenden Auflagen unvereinbar ist — und sie
bleibt überprüfbar, weil die Auflagen benannt sind, nicht das Ergebnis.

### K64 · Die Genehmigungskette: zwei Bruchstellen geschlossen, eine bleibt beim Speicher

UI-06 hatte drei Bruchstellen gemeldet. Nachgeprüft:

1. **`web(...)` fehlt** — **geschlossen.** Das Unterattribut existiert, die
   Kollisionsprüfung sitzt in der *Registry* (nicht im Makro, das je eine
   Operation sieht), und **14** Operationen tragen eine `Surface::Web`.
2. **`ApprovalRecord` trägt keine Umkehrbarkeit** — **bleibt offen, und zwar
   dort, wo sie hingehört.** `PendingApprovalView` nimmt `action_kind` und
   `irreversible` entgegen; `ApprovalRecord` selbst trägt nur
   `request/session/call_id/actor/issued_at`. Die fehlende Zeile ist benannt:
   `ApprovalRecord` bräuchte `action_kind` und `reversibility`, gesetzt bei
   `issue()`. **`harw-web` hat keine Attrappe gebaut, die so tut, als wüsste
   sie die Aktion** — das wäre die schlechtere Antwort gewesen, weil
   „Prozessbaum beenden" und „cgroup einfrieren" als Knopf gleich aussehen und
   es nicht sind.
3. **Keine Einzelabfrage einer offenen Anfrage** — **entschieden statt
   umgangen.** `Surface::Web { path, readonly, approval }` trägt weder
   Pfadparameter noch Rumpf; GET-Routen sind feste Pfade. Eine **Liste** aller
   offenen Anfragen passt in dieses Modell und genügt: es sind wenige, und der
   Bediener sieht sie ohnehin alle. Eine Einzelabfrage bräuchte eine
   Erweiterung von `Surface::Web`.

**Der eigentliche Befund war der vierte, und er ist beantwortet.** Woher der
`ApprovalActor` kommt: ausschließlich aus `ApprovalActorResolver::actor_for`,
angewandt auf die per `SO_PEERCRED` vom **Kernel** gelieferten
`PeerCredentials`. `resolve_approval` hat in der Signatur keinen
`actor`-Parameter, der aus einem Client-Wert stammen könnte, und ein Test hält
genau das fest.

**Und die Antwort auf „reicht `authz.rs`' Tabelle für beide Achsen?" ist
nein — mit einer Begründung, die zählt.** `StaticUidTierMap` bildet uid →
`PermissionTier` ab und hat einen sinnvollen Rückfall: *unbekannt heißt
Observer*. Die Genehmiger-Tabelle bildet uid → `ApprovalActor` ab und hat
**bewusst keinen Default** — ein unbekannter Genehmiger, der auf einen
Standard-Actor fiele, unterliefe die Actor-Bindung von `ApprovalStore::resolve`
vollständig. **Zwei Tabellen, dasselbe Kompositionsprinzip: serverseitig
gesetzt, vor der Anfrage, nie aus ihr.**

*Offen und als eigener Knoten vergeben:* die Fläche ist gebaut und von außen
**nicht erreichbar** — es fehlen die zwei Operationen `approval.pending` und
`approval.resolve`. Dass `approval.resolve` selbst `approval = "none"` trägt,
ist keine Nachlässigkeit: eine Operation, die eine Bestätigung entgegennimmt,
kann nicht selbst eine verlangen.

### K65 · Der OTLP-Sink sendet echt — und bringt eine Falle für die Kompositionswurzel mit

`harw-observe-otlp` war einer der beiden Sinks ohne Verdrahtung (K60). Jetzt
trägt er einen echten HTTP-Transport. Drei Entscheidungen daran verdienen
festgehalten zu werden:

**Kein neues Crate.** Gewählt wurde `hyper` + `hyper-util` +
`http-body-util` + `bytes` + `tokio` — **null neue Auflösungen in
`Cargo.lock`**. `harw-mcp-server` und `harw-web` ziehen exakt dieselben
Fassungen bereits für ihre Serverseite, und `harw-provider-http`s `reqwest`
baut ohnehin auf `hyper`s Client-Maschinerie. Die zusätzlichen
Feature-Schalter vereinigen nur Merkmale auf bereits aufgelösten Paketen; der
Knoten hat das paketweise gegen `Cargo.lock` geprüft, statt es anzunehmen.
`reqwest` wurde als **zweiter, größerer HTTP-Stapel** abgelehnt.

**TLS wurde nicht gebaut, und `https://` wird abgelehnt statt herabgestuft.**
`rustls` bräuchte ein Zertifikatsbündel, `native-tls` ein `build.rs` und
verletzt D3. Die Ablehnung sitzt in `HttpTransport::new` — **nicht erst beim
Verbindungsaufbau**. Ein Startfehler ist besser als ein Sink, der zur Laufzeit
still nichts sendet.

**Die Falle, und sie betrifft nicht diesen Knoten, sondern den nächsten.**
`flush()`/`send_batch` sind synchron nach Vertrag. `HttpTransport::new` baut
sich deshalb einen **eigenen einthreadigen `tokio::runtime::Runtime`** und
überbrückt mit `Runtime::block_on`. Wird `flush()` aus einem **bereits
laufenden** Runtime aufgerufen, **panickt `block_on`** — kein Fehlerwert, den
ein Aufrufer behandeln könnte, sondern ein Absturz.

Der Knoten hat das dokumentiert statt es zu verschweigen, und die Auflage
benannt: eine Aufrufstelle im async-Kontext gehört in `spawn_blocking`. **Die
Entscheidung liegt bei der Kompositionswurzel**, weil nur dort bekannt ist, in
welchem Kontext der Sink läuft — sie wurde dem zuständigen Knoten während
seiner Arbeit zugestellt.

*Nebenbefund mit dem richtigen Vorzeichen:* für die Fehlerzählung wurde
**kein neuer Zähler** gebaut. `OtlpSink::flush` zählt `Send`-Fehler bereits
über `send_failure_count`; der Knoten hat das nachgelesen, statt daneben einen
zweiten zu stellen. Und die Zustelltests öffnen **keinen Fremdrechner**: jeder
startet seinen eigenen `TcpListener` auf `127.0.0.1:0` und antwortet mit einem
handgeschriebenen HTTP/1.1-Responder — kein zweiter Server, kein Netz.

**Die Adapter-Isolation hält:** die einzigen öffentlichen Signaturen sind
`new(&OtlpConfig) -> Result<Self, OtlpError>` und
`send_batch(&self, &[u8]) -> Result<(), OtlpError>`. Kein `hyper`- oder
`tokio`-Typ tritt nach außen; `Debug` ist von Hand geschrieben, damit keine
Kopfzeilenwerte durchsickern.

### K66 · `validate_patch` fehlt nicht der Aufrufer, sondern die Eingabe

K36 hielt fest, dass `harw_plan::admission::validate_patch` workspace-weit
keinen Aufrufer hat, und dass die Metrik `scope_violation_rate` deshalb
bewusst weggelassen wurde. Der Knoten, der die Verdrahtung bauen sollte, hat
zuerst gesucht — und **die Fehlanzeige geliefert statt eine Zeile einzubauen.**

**Nachgemessen** über alle `.rs`-Dateien nach `UnifiedDiff`, `PatchFile`,
`validate_patch` und `MutationContract`: die einzigen Treffer sind
`harw-plan/src/admission.rs` selbst, drei Dateien in `harw-plan-bridge`, die
den `MutationContract` **bauen** oder ihn nennen, und
`harw-cli/src/job_worker.rs`.

**Der Befund ist tiefer als „kein Aufrufer".** Der eine echte Konsument des
Kontrakts ist `derive_plan_node_sandbox` in `harw-cli/src/job_worker.rs`,
erreichbar über einen echten Produktionspfad
(`execute_plan_node_claim` → `PlanNodePayload::parse` →
`derive_plan_node_sandbox`). Aber er prüft **etwas anderes**: er bildet
`allowed_paths` auf eine einzige binäre Berechtigung ab
(`Permission::WriteWorkspace` an oder aus) und schränkt damit den **ganzen**
Workspace ein.

**Nirgends im Baum entsteht eine Liste einzeln geänderter Dateien.** Kein Code
baut je einen `UnifiedDiff`. Die Granularität, die `validate_patch` anbietet,
wird deshalb nicht bloß nicht aufgerufen — **sie hat keine Eingabe.**

**Es gilt:** die Prüfung gehört nicht nach `harw-plan-bridge`. Der Job-Payload,
den diese Crate erzeugt, trägt den Kontrakt, aber nie die geänderten Dateien;
eine Prüfung dort hätte **nie einen echten Patch durchlaufen** — genau die
Attrappe, die im Bericht wie eine Verdrahtung aussieht.

Die richtige Stelle ist benannt: **`harw-cli/src/job_worker.rs`, als neuer
Schritt zwischen Turn-Ende und `report_plan_node_outcome`**, der die
tatsächlich geänderten Dateien des Sandkastenverzeichnisses zu einem
`UnifiedDiff` zusammenfasst und ihn gegen den mitgeführten `MutationContract`
prüft, **bevor** der Job als erfolgreich zurückgemeldet wird.

`scope_violation_rate` bleibt bis dahin undeklariert. **Der Grund ist
unverändert der aus K36:** null wäre als „keine Verletzungen" gelesen worden,
nicht als „nie geprüft".

*Der allgemeine Punkt, und er verschärft die vierte Spalte der Abnahme:*
„Konsument existiert?" ist die richtige Frage, aber nicht die letzte. Eine
Funktion kann einen Aufrufer bekommen und trotzdem wirkungslos bleiben, wenn
**niemand den Wert herstellt, den sie prüfen soll**. Die Frage danach lautet:
*existiert die Eingabe, auf die diese Prüfung wartet?*

### K67 · Der `#[context_provider]`-Befund war behoben — und die Nachmessung fand einen größeren

Der Plan führte offen: *das Makro erzeugt keine
`ContextProvider::namespace()`/`max_trust()`-Überschreibungen, makro-erzeugte
Anbieter zeigen dem geprüften Registrierungsweg die Vorgabewerte.* Der Knoten
hat nachgesehen und die Reparatur **bereits im Baum** gefunden:
`expand_context_provider` erzeugt beide Überschreibungen aus `Self::NAMESPACE`
und `Self::TRUST`, zusätzlich zum bestehenden `DeclaredContextProvider`. Der
Eintrag war veraltet, nicht falsch — und der Knoten hat nichts umgeschrieben,
was schon stand.

**Die Richtung des ursprünglichen Fehlers ist beantwortet, und sie entlastet.**
`ContextProvider::max_trust()` hat als Trait-Vorgabe `TrustClass::Data` — die
**niedrigste** Klasse. Ein Anbieter, der `trust = Instruction` deklarierte,
wurde also als `Data` gemeldet: **unter**-vertrauenswürdig, nie über. Der
Namensraum fiel auf `std::any::type_name::<Self>()` zurück — eindeutig, nur
nicht der deklarierte. **Beide Fälle sind falsch-streng, keiner ist eine
Eskalation.** Das ist die Frage, die vor der Dringlichkeit steht, und sie war
hier gutartig zu beantworten.

**Die Nachmessung hat den größeren Befund geliefert.** Der Auftrag sprach von
„zwanzig Crates, die dieses Makro benutzen". Nachgezählt: **genau eine**
produktive Anwendung im ganzen Baum —
`harw-project-discovery/src/provider.rs:79`, Funktion `project_context`. Alle
übrigen Fundstellen in `harw-plan-bridge` und `harw-extension-api` sind
**Doku-Kommentare**, keine Makro-Anwendungen.

Und diese eine Stelle deklariert **weder Namensraum noch Vertrauensmaß**. Sie
verhält sich nach der Reparatur unverändert — was heißt: **die reparierte
Fähigkeit hat heute keinen einzigen Nutzer.**

**Das ist derselbe Fehlertyp wie K51**, wo aus einem `grep`-Muster mit `::` in
der Mitte eine Zahl in den Plan wanderte, die um Größenordnungen danebenlag.
Hier war es meine Zahl im Auftrag: zwanzig Crates **nennen `harw-macros` in
ihrer `Cargo.toml`** (K18, korrekt gemessen) — aber sie benutzen andere
Makros dieser Crate. **Eine Zahl über eine Crate ist keine Zahl über eines
ihrer Makros.**

*Was daran für die Abnahme zählt:* die Vertrauensdeklaration ist jetzt
durchgängig — vom Attribut über beide Registrierungswege bis zur geprüften
Stelle. Aber die vierte Spalte steht auf **nein**: kein Anbieter im Baum
deklariert etwas anderes als die Vorgabe. Der Weg ist gebaut und leer.

### K68 · Der eBPF-Lader ist echt — und `aya` war nicht das Hindernis, für das ich es hielt

Der Plan führte `harw-dod-bpf` mit der Auflage „keine `aya`-Typen in
öffentlichen Signaturen" und der offenen Frage, ob die Crate wegen ihres
Gewichts überhaupt nach AW7 gehört. Der Knoten hat **gegen die gepinnte
Quelle** geprüft statt gegen Werbeversprechen oder docs.rs:

| Frage | Befund, Quelle: GitHub-Tag `aya-v0.14.0` |
|---|---|
| `build.rs`? | **Keines.** Kein `[build-dependencies]`, kein Bauskript. D3 hält. |
| `unsafe` in der benutzten Fläche? | **Keines.** `Ebpf::load`, `TracePoint::load`/`attach`, `KProbe::load`/`attach`, die `TryFrom<&mut Program>`-Umwandlungen und `RingBuf::next` sind sämtlich sicheres Rust. |
| Transitive Crates | **17**, mehrere davon bereits im Baum |
| `TracePoint::attach` | `(&mut self, category: &str, name: &str) -> Result<TracePointLinkId, ProgramError>` — **schlichte Zeichenketten**, keine Tupelüberraschung wie bei `rustix::recv` (K45) |

**Zur Aussage „`aya` ist nicht `#![forbid(unsafe_code)]`":** das ist richtig
und **irrelevant**. Die Workspace-Auflage bindet den Code *dieser* Crate, nicht
den ihrer Abhängigkeiten — sonst wäre jede Zeile, die `libc` berührt,
verboten. Der Knoten hat die Unterscheidung ausdrücklich gezogen, statt sie zu
übergehen oder daran zu scheitern.

**Was die Entscheidung möglich gemacht hat, war nicht `aya`, sondern eine
frühere Korrektur an anderer Stelle:** das Hindernis war der
`SocketFilter`-Weg über einen offenen Socket. Seit beide Sensoren über
**Tracepoints** arbeiten, ist es weg. `SocketFilter` liefert jetzt
`BpfError::UnsupportedProgramKind` **mit der Bedingung, unter der es wieder
aufgemacht würde**, im Code notiert.

**Die verbleibende Grenze ist benannt und nicht überspielt:** ein echter Lader
ist etwas anderes als ein erzeugtes eBPF-Objekt. Die ELF-Erzeugung braucht eine
**zweite Werkzeugkette** (`aya-ebpf`, `nightly`, Ziel `bpfel-unknown-none`) und
ist ein eigener Bauschritt — dokumentiert als künftiger Knoten, nicht als
erledigt gezählt.

`CAP_BPF` wird **vor** dem ersten `aya`-Aufruf über `/proc/self/status`
geprüft, mit reinem `std` und ohne `unsafe` — dieselbe fail-closed-Richtung wie
bei Landlock (Entscheidung Nr. 4).

*Und ein Nebeneffekt, der den Weg über die Hauptsitzung genommen hat:* die drei
neuen Fehlervarianten (`ProgramLoadFailed`, `UnsupportedProgramKind`,
`UnknownHandle`) machen ein `match` in `harw-dod-procmon/src/sensor.rs`
nicht-erschöpfend. Die Datei gehört einem **anderen, gleichzeitig laufenden**
Knoten; die Meldung ging deshalb an ihn, statt dass ihm jemand die Datei unter
den Händen ändert — samt der Auflage, die drei Varianten **einzeln** zu
behandeln und nicht in ein `_ =>` fallen zu lassen. Ein Sammelzweig nähme genau
die Compilerwarnung weg, die den Fall diesmal sichtbar gemacht hat.

### K69 · fanotify ist gebaut — die Antwort lag in einer Crate, die schon im Baum stand

Der Plan gab `harw-probe-fs` die Auflage „fanotify auf `rustix`" und ließ drei
Ausgänge offen, darunter eine dokumentierte Ablehnung. Der Knoten hat die
Voraussetzung **an der gepinnten Quelle** geprüft und bestätigt gefunden:
`rustix-1.1.4/src/not_implemented.rs`, Zeilen 298–299, führt `fanotify_init`
und `fanotify_mark` weiterhin unter `pub mod yet { not_implemented!(...) }`.
**Der im Plan vorgesehene Weg existiert nicht.**

**Die Antwort war `nix`, und `nix 0.29` stand bereits in `Cargo.lock`.**
Recherchiert, nicht geraten: `nix::sys::fanotify` landete in **0.28.0**
(2024-02-24) und übernahm in **0.30.0** die I/O-Sicherheit
(`BorrowedFd`/`OwnedFd`). Der Knoten hat die tatsächliche Quelle des Tags
`v0.31.3` gelesen — 446 Zeilen — und für jede benutzte Funktion einzeln
bestätigt, dass sie **sicheres Rust** ist: das crate-interne `unsafe` taucht in
keiner Signatur auf, die dieser Code aufruft.

`fanotify-rs` wurde als dünner `libc`-Wrapper verworfen; `inotify` als Rückfall
ebenfalls, **mit Begründung**: es verlöre `loginuid` und die Abdeckung eines
ganzen Dateisystems — und beides ist der Zweck des Sensors.

**Drei Entscheidungen im Gebauten verdienen festgehalten zu werden:**

- **Markierung je Dateisystem, nicht je Inode** (`FAN_MARK_FILESYSTEM`).
  Inode-Markierungen sind **nicht rekursiv**; wer einen Verzeichnisbaum
  überwachen will und je Inode markiert, überwacht die Wurzel und sonst nichts.
- **Der `timeout`-Parameter wird eingehalten**, über `rustix::event::poll`,
  statt ignoriert zu werden. Ein Parameter, den niemand liest, ist von einem
  fehlenden nicht zu unterscheiden.
- **Die `uid` kommt nicht von fanotify** — es meldet keine. Sie wird über den
  Eigentümer von `/proc/<pid>` aufgelöst, das Ziel über `/proc/self/fd/<n>`.
  Das steht am Ort, damit niemand später eine Genauigkeit annimmt, die die
  Quelle nicht liefert.

**Kein `unsafe`, und kein Test öffnet einen echten fanotify-Deskriptor** —
geprüft werden nur die reinen Flag- und Pfadhelfer.

*Zwei Beobachtungen für die Verifikation:* `harw-dod-fsmon` wurde
**ausschließlich in der Dokumentation** angefasst — die Bindung bleibt im
privilegierten Binary, wie die Architekturtrennung es vorsieht. Und der Baum
trägt jetzt **zwei `nix`-Fassungen**: `0.29.0` transitiv aus einer bestehenden
Abhängigkeit, `0.31.3` direkt für fanotify. Semver-inkompatibel, also
koexistierend — kein Fehler, aber eine Doppelung, die beim nächsten
Abhängigkeits-Durchgang zusammenfallen sollte.

### K70 · `DetailMode` erreicht die IR — und die Vererbungsfrage hatte eine andere Antwort als die Decke

Die Kette zu `DetailMode::References` hatte vier Blocker. Zwei sind zu.

**1. `ContextProgram` trägt jetzt den `DetailMode`.** Neues Feld
`section_detail: Vec<SectionDetail>` — Sektionsname plus
`harw_context::DetailMode`, reihenfolgetreu und für **alle** Sektionen,
anders als `must_include`. `from_resolved_program` befüllt es; der freie
`[context]`-Konfigurationspfad liefert weiterhin leer, weil es dort keine
Pro-Sektion-Grammatik gibt — **das steht am Ort, statt still zu geschehen.**

**Die `SNAPSHOT_HASH_DOMAIN` ist von `v2` auf `v3` gestiegen**, mit
Begründung im Doc-Kommentar: ein neues Feld auf einer bereits gehashten
Teilstruktur verschiebt **jeden** Digest. Nachgeprüft: die bestehenden
Stabilitätstests prüfen **relative** Gleichheit über wiederholtes Absenken,
**kein Test hält ein festes Hash-Literal** — sie bleiben grün. Ein
stillschweigend geänderter Hash wäre schlimmer als ein erklärter.

**2. Die Vererbungsfrage ist beantwortet: mitbringen, nicht erben.** Eine
`ContextCeiling` wird **geschnitten**, weil sie eine Sicherheitsobergrenze ist
— ein Kind sieht nie mehr als sein Elternteil. Ein Kontextprogramm ist
**Auswahl innerhalb** dieser Grenze, keine Obergrenze; für einen Schnitt gibt
es kein Argument. Jede Rolle bringt ihr eigenes aus `harw-registry-defaults`
mit, und die Decke schneidet unverändert weiter.

**Der Befund daneben ist der interessantere.** Das Feld liegt auf
`AgentSession`, **nicht** auf `SpawnContext` — und zwar aus einem
Sichtbarkeitsgrund, nicht aus einem Entwurfsgrund: `SpawnContext`s Felder sind
**alle `pub`** und werden an über einem Dutzend Stellen außerhalb des
Schreibbereichs **per Literal** konstruiert (`child_controller.rs`,
`harw-tui/*`, `harw-cli/*`, mehrere `tests/`). Ein Pflichtfeld dort hätte
sie alle gebrochen. `AgentSession`s Felder sind privat, die einzige
Konstruktion liegt in derselben Datei — **sicher erweiterbar.**

*Das ist eine Aussage über den Bestand, nicht über diesen Knoten:* ein Struct
mit ausschließlich `pub`-Feldern und Literal-Konstruktion an dreizehn Stellen
ist gegen additive Erweiterung **verschlossen**, obwohl es offen aussieht. Die
automatische Durchreichung durch `ManagedAgentSpawner::admit` bleibt deshalb
ein Folgeknoten mit größerem Schreibbereich.

**3. und 4. bleiben offen, beide präzise verortet** — und beide sind als
eigene Knoten vergeben:

- **Der Weg zum `ContextLoadExecutor`.** `find_executor` liefert nur
  `Arc<dyn ToolExecutor>`. Die fehlende Zeile ist wörtlich benannt: eine
  Vorgabemethode `fn as_context_load_executor(&self) -> Option<&ContextLoadExecutor> { None }`
  auf `trait ToolExecutor`, überschrieben in `context_load.rs`. **Kein
  `downcast` eingebaut** — eine Typprüfung zur Laufzeit an einer Stelle, die
  sie nicht braucht, wird später niemand mehr los.
- **Kein `ContextProvider` erzeugt `harw_context::Fragment`.**
  `gather_context` liefert weiterhin die alte Form.

**Nach diesem Knoten erreicht kein Produktionspfad `FragmentReference`** — und
der Knoten sagt das als Erstes, statt die zwei geschlossenen Punkte als
Fortschritt zu verkaufen.

### K71 · Der produktionsreife Einbetter: das lokale Modell scheitert an 171 Crates, nicht an der Idee

`harw-lens-embed` bot bis hierher nur `DeterministicEmbedder`, laut eigener
Doku für Tests gedacht (K56). Der Knoten hat beide Wege **gemessen**, nicht
abgewogen:

**Lokales Modell (`candle` + `tokenizers`), nachgezählt in einem
Wegwerf-Projekt:** **171 zusätzliche Crates** — gegen 665 im gesamten
heutigen Workspace. `candle-core` selbst hat kein `build.rs`, aber
`tokenizers` zieht `onig`/`onig_sys`: eine C-Bibliotheksbindung **mit
`build.rs`**, die zur Bauzeit einen C-Übersetzer braucht. Dazu Modellgewichte
in dreistelliger Megabytezahl, für die es keinen Bezugs- und Ablageweg gibt.
**Doppelter D3-Verstoß**, und die Zahl macht ihn unbestreitbar.

**Der Fernweg, und dabei ein Befund über die vorgeschlagene Naht:**
`harw-provider-http` — im Auftrag als Weg genannt — hat **überhaupt keinen
Einbettungs-Endpunkt**. Es implementiert nur `ModelProvider::respond`, und es
ist durchgängig async, während `Embedder::embed` synchron nach Vertrag ist.
**Die vorgeschlagene Naht existiert nicht.** Der Knoten hat das gemeldet und
einen eigenen, kleinen Transport in `harw-lens-embed` gebaut — mit **genau der
`reqwest`-Fassung und Merkmalswahl**, die `harw-provider-http` bereits
auflöst, also ohne neue Auflösung.

**Die Ehrlichkeit auf Typebene hält.**
`RemoteEmbedder<HttpEmbedBackend>::locality()` bleibt **fest** auf
`Locality::Remote` — der neue Rückwärtsteil **kann darüber nicht lügen**. Der
wichtigste Test belegt, dass `route(Confidential)` selbst bei registriertem,
echtem HTTP-Profil weiterhin den lokalen Einbetter wählt.

**Was es nicht kann, steht zuerst:** `Confidential` hat **weiterhin keinen
produktionsreifen Einbetter**. Der HTTP-Weg kann ihn strukturell nie bedienen,
ein lokales Modell existiert nicht im Baum.

**Und dieselbe Blockier-Falle wie beim OTLP-Sink (K65):**
`reqwest::blocking` blockiert den aufrufenden Faden; aus einem laufenden
Tokio-Kontext heraus **panickt** es. **Zwei unabhängige Knoten sind in dieselbe
Form gelaufen** — ein synchroner Vertrag über einem asynchronen Transport.
Das ist kein Zufall mehr, sondern eine Eigenschaft dieses Baums: wo ein
`Sink`- oder `Embedder`-Trait synchron ist und der Transport es nicht, muss
die **Kompositionsstelle** `spawn_blocking` entscheiden. Beide Fälle sind am
Ort dokumentiert.

*Die verbleibende Kante ist vergeben:* `harw-tool-lens` kann den neuen
Einbetter **nicht erreichen** — die kuratierte Fassade `harw-lens` exportiert
ihn bewusst nicht, und `harw-tool-lens` hängt gar nicht an
`harw-lens-embed`. Der Knoten hat deshalb nur die Moduldoku korrigiert, damit
sie **„über den heutigen Abhängigkeitsgraphen unerreichbar"** sagt statt
„es gibt keine Alternative" — und den Platzhalter im Modellnamen stehen
lassen, **weil er benutzt wird**. Ein falscher Name wäre schlimmer als ein
hässlicher.

### K72 · Zwei Geschwister sind angeglichen — und `observe()` blieb aus einem Grund

K52 hielt fest, dass `harw-dod-procmon` und `harw-dod-flow` sich verschieden
entschieden hatten und der einzige Konsument procmons Form für richtig hielt.
Beides ist jetzt aufgelöst.

**`harw-dod-flow` implementiert `Sensor`** — nach procmons Muster, nicht nach
flows Bedenken. Die Bedenken waren richtig und die Konsequenz war es nicht:
`sensor_suite!` **nicht** zu benutzen ist die Antwort, `Sensor` nicht zu
implementieren war eine zweite. Der Grund steht am Ort und ist der aus K41:
ein Sensor, der `handle.scope()` nie anfasst, besteht alle sechs Prüfungen,
**ohne seine Parselogik je auszuführen**.

**`observe()` ist geblieben, und die Begründung trägt:** es arbeitet auf
**einem** bereits beschafften Ereignis und kennt keinen Lader; `poll()` fügt
die Beschaffung hinzu, sammelt mehrere und bildet Lade- und Parsefehler auf
`SensorError` ab. **`poll()` ist buchstäblich aus `observe()` gebaut** — nicht
dieselbe Sache unter zwei Namen. Ein Test hält fest, dass beide für dieselbe
Roheingabe dasselbe Ereignis liefern.

**`harw-dod-procmon` hat sein Gegenstück bekommen:**
`PROCMON_TRACEPOINT_ATTACH_POINT` und `procmon_program_spec(...)`, in derselben
Form wie `flow_program_spec()`. Der Konsument muss es nicht mehr **aus der Doku
abschreiben** — genau der Fehlertyp aus K40, wo eine Sicherheitsregel ihre
Einstufung aus Prosa rekonstruierte.

**Die drei neuen `BpfError`-Varianten aus K68 sind in beiden Crates einzeln
behandelt**, ohne `_ =>`-Sammelzweig, jede mit begründeter Dauerhaftigkeit:
`UnsupportedProgramKind` bleibt unbekannt, `ProgramLoadFailed` entsteht nur in
`load()`, das diese Sensoren nie selbst aufrufen, und `UnknownHandle` betrifft
einen Griff, den der Sensor sein ganzes Leben lang unverändert hält — **jeder
weitere Aufruf scheitert identisch.** Alle drei sind `Permanent`, und Tests
halten das fest.

*Bemerkenswert am Ablauf:* die Meldung über die neuen Varianten kam von einem
**anderen, gleichzeitig laufenden** Knoten und lief über die Hauptsitzung, weil
die betroffene Datei diesem hier gehörte. **Das ist die Form, in der
Vertragsdrift zwischen Parallelknoten aufgelöst gehört** — nicht dadurch, dass
zwei Agenten dieselbe Datei anfassen.

### K73 · Dieselbe Fehlerabbildung stand in drei Kopien — und alle drei brachen gleichzeitig

`bpf_error_to_sensor_error` existierte dreimal: in `harw-dod-procmon`, in
`harw-dod-flow` und in `harw-probe-bpf`. Die dritte Kopie trug ihren Grund im
Doc-Kommentar: *„für denselben Fehlertyp erneut geschrieben, weil diese private
Funktion dort nicht exportiert ist."*

Als `BpfError` in K68 um drei Varianten wuchs, **brachen alle drei zugleich** —
zwei davon fingen die Agenten ab, die die Dateien gerade hielten, die dritte
fand erst der zentrale `cargo check`. **Das ist keine Anekdote, sondern der
dritte Fall desselben Musters** nach der Punktgrenzen-Regel (K37) und dem
Verzeichnis-fsync (K49): eine Regel in Kopien driftet, und der Bruch tritt an
so vielen Stellen zugleich auf, wie es Kopien gibt.

**Aufgelöst:** `impl From<BpfError> for harw_dod_cap::SensorError` in
`harw-dod-bpf/src/error.rs` — die kanonische Heimat, weil `harw-dod-bpf`
bereits an `harw-dod-cap` hängt und `BpfError` dort zu Hause ist. Die drei
Funktionen bleiben als benannte Aufrufpunkte bestehen, damit vorhandene Tests
und Aufrufstellen unverändert weiterlesen — **sie tragen die Regel nicht
mehr.**

Die Zuordnung folgt jetzt an einer Stelle der **Dauerhaftigkeit**, mit einer
Unterscheidung, die zählt: `MalformedEvent` wird `MalformedSource`, **nicht**
`SourceUnavailable`. Die Quelle ist erreichbar, ihr Inhalt ist es nicht — und
davon hängt ab, ob ein Sentinel den Sensor abschaltet oder nur dieses eine
Ereignis verwirft.

*Ein zweiter Befund aus derselben Prüfung:* `ProbeError::BpfLoaderUnavailable`
war **nie mehr konstruierbar**, seit `build_real_loader` einen echten Lader
liefert statt immer zu scheitern. Eine Fehlervariante, die niemand erzeugen
kann, samt `Display`-Zweig und drei Tests, die sie prüfen. Entfernt, und die
zwei Doku-Stellen, die sie noch als „heute immer" führten, sind nachgezogen.
**`cargo check` hat sie gefunden, weil `dead_code` sie meldet — kein Test hätte
das gekonnt**, denn ein Test über einen unerreichbaren Zweig ist selbst grün.

### K74 · Die Verifikation nach acht parallelen Knoten: ein Fehler

Acht Knoten sind gleichzeitig gelandet — Kompositionswurzel, OTLP-Transport,
fanotify, eBPF-Lader, Netzdurchsetzer, Genehmigungskette, `DetailMode`-IR und
die Angleichung der zwei Geschwistercrates. Der erste zentrale
`cargo check --workspace` meldete **genau einen** Fehler: die dritte Kopie aus
K73.

**Das ist die Rechtfertigung des Contract-Masters**, und zwar in Zahlen: das
Vorprogramm erzeugte bei dreizehn Agenten **26 Fehler, alle Vertragsdrift**.
Hier waren es acht Agenten und ein Fehler — und der lag nicht an einer
erfundenen Signatur, sondern an einer **vorhandenen Vervielfachung im
Bestand**, die keine Vertragsfixierung hätte verhindern können.

Die Gates danach, in dieser Reihenfolge:

| Stufe | Ergebnis |
|---|---|
| `cargo check --workspace` | **0 Fehler, 0 Warnungen** |
| `cargo check --workspace --all-targets` | **0 Fehler, 0 Warnungen** |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | **Exit 0** |
| `cargo test --workspace` | **6599 bestanden, 0 gescheitert** (zuvor 6500) |

**Drei Clippy-Befunde, alle Doku**, und zwei davon in bekannten Fallen: ein
Absatz, der ohne Leerzeile auf eine verschachtelte Liste folgte und deshalb
als deren Fortsetzung gelesen wurde, und ein weiteres Mal die Hauskonvention
`- [\`Error::X\`]: Text` — Markdown liest sie als **Linkreferenzdefinition**,
wenn der Text danach wie ein Ziel aussieht. Hier stand `: siehe` am Zeilenende,
also genau ein Wort. **Die Konvention selbst ist der Auslöser**, nicht der
Autor; sie feuert an einer Handvoll von hunderten Stellen.

**Und ein Test, der genau seine Aufgabe erfüllt hat.**
`all_registered_ops_are_reachable_by_name` schlug fehl, weil die zwei neuen
Genehmigungs-Operationen registriert sind und die TUI sie nicht kennt. Sein
Doc-Kommentar begründete die Zählung mit: *„eine Operation, die `register_all`
einträgt und die TUI nicht kennt, wäre über keine Fläche erreichbar."*

**Das war wahr, solange es nur `Surface::Command` gab.** `approval.pending` und
`approval.resolve` sind reine Web-Flächen und haben in einem Terminal nichts zu
suchen — die TUI kennt sie **zu Recht** nicht.

Der Test ist deshalb **nicht abgeschwächt, sondern getrennt und verschärft**:
die Namensliste bleibt für Command-Operationen erschöpfend, und **zusätzlich
muss jede Operation mindestens eine Fläche tragen**. Eine Operation ganz ohne
Fläche wäre wirklich unerreichbar — und das fing die alte Zählung **nicht**,
weil sie Zahlen verglich statt Flächen. Ein Test, der seine eigene Annahme
überlebt hat, gehört korrigiert, nicht gelöscht.

### K75 · `contribute_v2` hat einen Produktionsaufrufer — und die Brücke hätte fast still verschärft

`gather_context` (`harw-core/src/turn_loop.rs`) ruft jetzt
`provider.contribute_v2(ctx)` statt `provider.contribute(ctx)`. Damit hat die
Naht aus dem Fragment-Knoten **einen echten Aufrufer**: jeder Turn durch
`drive_turn` geht für jeden registrierten Provider durch sie hindurch.

**Die Wahl des Wegs hing an einer Zahl.** `gather_context` ist öffentlich; der
Knoten hat die Aufrufer gezählt statt geschätzt: **genau einer**, `drive_turn`,
in derselben Datei. Damit war eine zweite Funktion daneben
(`gather_context_v2`) ausgeschlossen — sie wäre die vom Leitfaden §1 verbotene
Verdopplung **und** ohne Aufrufer sinnlos. Gewählt: intern auf v2, am Ende
verlustfrei zurück, weil `ContextFragment` nie mehr als `label` und `content`
trug und **kein Konsument** `section`, `trust`, `stability` oder `cost` liest —
nachgeprüft, nicht angenommen.

**Was der Knoten gemeldet statt verschwiegen hat**, und es ist die wichtigere
Hälfte: die Vorgabe-Brücke lief als
`.filter_map(|f| fragment_from_v1(f, …).ok())`. `fragment_from_v1` weist ein
**leeres Label** zurück, und `.ok()` verwarf das Fragment dann **spurlos** —
während der alte Sammelpfad es seit jeher unter `"unlabeled"` durchgereicht und
nur gegen die Aktivierung geprüft hatte.

Der Knoten hat nachgemessen, dass heute kein Provider und kein Test ein leeres
Label erzeugt, und daraus richtig geschlossen, dass **keine beobachtbare
Regression** vorliegt. **Das genügt nicht als Begründung, es so zu lassen.**
Ein spurlos verworfenes Fragment ist von einem nie beigesteuerten nicht zu
unterscheiden; der Zeitpunkt, an dem jemand ein leeres Label liefert, ist
genau der, an dem niemand mehr weiß, warum sein Fragment fehlt.

**Zentral behoben:** `V1_UNLABELED` steht in
`harw-extension-api/src/v1_compat.rs` — derselbe Wert, den `gather_context`
immer benutzt hat —, und die Vorgabe von `contribute_v2` normalisiert ein
leeres Label darauf, **bevor** die Brücke greift. Der Übergang auf v2
verschärft das Verhalten damit nicht.

*Der allgemeine Punkt:* `.ok()` auf einem `Result` in einem `filter_map` ist
die kürzeste Schreibweise für „hier darf etwas verschwinden, und niemand
erfährt es". Sie ist an einer **Brücke zwischen zwei Datenformen** besonders
teuer, weil dort per Definition Werte durchgehen, die die neue Form nicht
kennt.

**Was weiterhin offen bleibt, unverändert benannt:** `Assembly::gather` in
`context_budget.rs` hat **weiterhin keinen Produktionsaufrufer** —
`gather_context` wandelt sofort auf `ContextFragment` zurück. Der volle
Anschluss der Montage bräuchte `ModelRequest::with_context_budget` in
`harw-core/src/model.rs`. Der Knoten hat den additiven, in seinem Bereich
baubaren Teil gebaut und den größeren Umstieg **nicht angefangen** — ein halb
umgestellter Montagepfad wäre schlechter als ein unveränderter.

### K76 · `SnapshotId`: der Unterschied zwischen „sieht aus wie ein Digest" und „ist der Digest dieses Inhalts"

Zwei Knoten in verschiedenen Teilsystemen hatten unabhängig gemeldet, dass
`SnapshotId` keine Grenze überqueren kann — kein `Serialize`/`Deserialize`,
kein öffentlicher Konstruktor. Die Verwaltungsfläche behandelte sie deshalb als
`string | null` und **nannte den strukturellen Grund**, statt einen Wert zu
erfinden.

**Die Falle war nicht die fehlende Serde-Ableitung, sondern was ein naives
Nachrüsten bedeutet hätte.** `SnapshotId` entsteht aus `compute_snapshot_id`
als Hash über die abgesenkte Programmfassung. Ein Konstruktor aus freiem Text
macht daraus einen **behaupteten** statt eines **berechneten** Werts — und wer
ihn einliest, hält etwas in der Hand, das aussieht wie ein Digest und keiner
ist.

Der Knoten hat drei Wege abgewogen und den teuersten gewählt, mit einer
Begründung, die den Ausschlag gibt:

| Weg | Warum nicht |
|---|---|
| `Deserialize` ableiten, keinen Konstruktor | **Serde kennt keine Geschäftsregel.** Jeder wohlgeformte Text wäre stillschweigend als berechneter Wert durchgegangen — die Ableitung *ist* der Konstruktor. |
| Ein formprüfender Konstruktor | Fängt Tippfehler, **nicht das eigentliche Risiko**: einen wohlgeformten Digest aus einer **anderen Hash-Domäne**, der zufällig wie ein aktueller aussieht. |
| **Gewählt:** zwei Typen | Kostet einen Typ und einen ausdrücklichen Bestätigungsschritt — und verwechselt „sieht aus wie" nie mit „ist". |

**Umgesetzt:** `SnapshotId` bleibt nur intern konstruierbar und bekommt
`Serialize` in der Form `{ "domain": …, "digest": … }`. Daneben steht
`ReferencedSnapshotId` mit `Serialize`/`Deserialize`, `parse()` (Länge 64,
Kleinbuchstaben-Hex, nicht-leere Domäne) und **`confirm(&SnapshotId) ->
Option<SnapshotId>`**.

**Zwei Eigenschaften daran zählen:**

1. **Die Domänenfassung ist ein sichtbares Feld**, nicht nur implizit im
   Hash-Inhalt. Ein `v2`-Verweis ist von einem `v3`-Verweis **am `domain`-Feld**
   unterscheidbar, unabhängig vom Digest-Text. Das ist genau der Fall, für den
   `SNAPSHOT_HASH_DOMAIN` existiert — und er wurde erst durch K70 real, als das
   neue Feld `section_detail` jeden Digest verschob.
2. **Eine eingelesene Identität ist von einer berechneten strukturell
   unterscheidbar.** Nur der Typ `SnapshotId` gilt als bestätigt;
   `ReferencedSnapshotId` bleibt bis zum `confirm()`-Aufruf eine **Behauptung**.

Das ist dieselbe Bewegung wie bei `Finding<S>` (K25) und `Action<Authorized>`:
**der Besitz des Wertes ist die Berechtigung**, und der Typ trägt die Regel,
statt sie einem Reviewer aufzubürden. Ein Test hält fest, dass ein
`v2`-Verweis mit **identischem Digest-Text** nie bestätigt.

*Was ein Konsument jetzt kann:* eine gespeicherte oder aus einem Vorschlag
stammende Kennung deserialisieren, ihre Domänenfassung **anzeigen** statt zu
raten, und sie — sobald die zugehörige IR neu abgesenkt ist — bestätigen oder
als „fremde Domäne" bzw. „Inhalt weicht ab" einordnen.

### K77 · Die `TrustClass` erreicht die Web-Fläche — und der Verlustpunkt hat jetzt eine Zeilennummer

Der Plan führte als schwersten offenen UI-Befund: *`harw-web` liefert die
`TrustClass` nicht; die Zwei-Block-Trennung aus AW4-01 ist in der UI
unsichtbar, die CSS-Token liegen bereit und sind unbenutzt.* Der Knoten hat
zuerst gesucht, wo die Klasse verlorengeht, und die Stelle **wörtlich**
benannt:

```rust
// harw-operations/src/operation.rs:714-717
pub struct OpOutput {
    pub text: String,
}
```

**`OpOutput` trägt nur Text.** Die Crate liegt außerhalb des Schreibbereichs,
und der Knoten hat dort **keine Klasse erfunden** — er hat den Verlustpunkt in
der Moduldoku von `harw-web/src/events.rs` festgehalten und `harw-operations`
nicht angefasst.

**Gebaut, additiv:** `WebEventKind::OperationCompleted` und der direkte
HTTP-Antwortrumpf tragen jetzt ein `trust`-Feld, mit `#[serde(default)]` (ein
Bestandsdatensatz bleibt lesbar) und `deny_unknown_fields` auf dem Enum — die
Klasse gehört zu den von außen deserialisierten Wire-Typen, für die K19 die
Regel setzt.

**Beide Aufrufstellen setzen ausdrücklich `TrustClass::Data`, die niedrigste
Klasse**, mit einem Kommentar, der auf `OpOutput`s fehlendes Feld zeigt. Nie
eine erfundene höhere.

**Die ehrliche Bilanz, die der Knoten selbst zieht:** in der Praxis wird nahezu
alles als `Data` gerendert. `OpOutput` trägt keine Klasse, und von **33**
`EvidenceRef`-Konstruktionsstellen setzen **zwei** einen Digest (K54) — es gibt
heute **keinen Pfad**, auf dem `harw-web` zu Recht `Evidence` oder
`Instruction` behaupten könnte.

**Das ist kein Mangel dieser Änderung, sondern ihr Zweck.** Vorher sah der
Bediener dreimal dasselbe und wusste nicht, dass er es sah. Jetzt sieht er,
dass alles `Data` ist — und das ist die Wahrheit über den heutigen Stand.

**Warum die Wurzel nicht in einem Zug mitgezogen wurde.** Ein Klassenfeld auf
`OpOutput` verschöbe die Frage nur: solange keine Operation etwas anderes als
`Data` **herstellen** kann, trüge das neue Feld überall denselben Wert. Die
wirksame Stelle ist K54 — die 31 `EvidenceRef`-Konstruktionsstellen ohne
Digest, verteilt über mehrere Crates. **Das ist kein disjunkter Schreibbereich
für einen Knoten**, und ein zu großer Knoten scheitert (K38).

**Verbindlich für die Abnahme:** die Kette lautet
*Digest an der Konstruktionsstelle* → *`TrustClass::Evidence` vergebbar* →
*`OpOutput` trägt eine Klasse* → *die Oberfläche rendert zwei Blöcke*. Die
letzten beiden Glieder sind gebaut; die ersten beiden sind **eine** Aufgabe und
gehören in einen eigenen Knoten mit dateigenauem Schreibbereich.

*Was die Oberfläche noch tun muss, gemeldet statt angefasst:* `trust` aus
`WebEventKind::OperationCompleted` und aus dem JSON-Rumpf lesen und über die
bereitliegenden CSS-Token in den Anweisungs- gegen den Datenblock rendern.
`webui/**` gehört diesem Knoten nicht.

### K78 · Der Einbetter erreicht das Werkzeug — und die Blockier-Falle war real

`harw-tool-lens` konnte den echten entfernten Einbetter nicht erreichen: die
kuratierte Fassade `harw-lens` exportierte ihn nicht, und die Crate hing gar
nicht an `harw-lens-embed`. Beides ist geschlossen — **und der Knoten hat
dabei einen Absturz verhindert, den niemand gesucht hatte.**

**Die Fassade exportiert jetzt drei Typen, jeden begründet:** `RemoteEmbedder`
und `HttpEmbedBackend`, weil der Aufrufer zwischen Platzhalter und echtem
Modell **wählen** muss, und `DimensionCheckedEmbedder` — **zwingend**, weil ein
Einbetter mit falscher Vektorlänge sonst einen Index **unbemerkt zerstört**.
Bewusst **nicht** exportiert: das `RemoteEmbedBackend`-Trait. Niemand
implementiert einen eigenen Transport; gebraucht wird die fertige
Implementierung.

**Umgestellt, mit unveränderter Vorgabe.** Ohne konfigurierten Endpunkt bleibt
es exakt beim `DeterministicEmbedder` und beim Modellnamen
`"deterministic-placeholder-32"` — ein Test bindet das fest. Mit Endpunkt
kommen Modellname und Dimension aus dem Katalogeintrag für
`EmbeddingRole::Query`, **nie** aus `Confidential`.

**Und die wichtigste Entscheidung ist die über den Fehlerfall:** scheitert
irgendetwas — Katalog nicht ladbar, keine Query-Rolle —, fallen **Einbetter und
Provenienz gemeinsam** auf den Platzhalter zurück. Damit kann die gemeldete
Provenienz nie vom tatsächlich benutzten Einbetter abweichen. Eine Provenienz,
die ein Modell nennt, das gar nicht gerechnet hat, wäre schlimmer als gar
keine — und die Prüfung aus K43 würde sie nicht fangen, weil sie **richtig
aussieht**.

**Die Blockier-Falle aus K65/K71 war hier real, nicht theoretisch.**
`lens_ask` ist `async fn` und rief die synchrone `ask_with_home` — und damit
`ask_embedder()` — **direkt auf demselben Tokio-Worker-Faden** auf. Mit
aktivem Fernpfad wäre das mit *„Cannot start a runtime from within a runtime"*
abgestürzt. Der Aufruf liegt jetzt in `tokio::task::spawn_blocking`;
`ask_with_home` bleibt vollständig synchron, bestehende Tests bleiben
unberührt.

**Zwei unabhängige Knoten hatten diese Form gemeldet, ein dritter ist
tatsächlich hineingelaufen.** Das ist der Punkt, an dem aus einem Muster eine
Regel wird: **wo ein Trait synchron ist und sein Transport es nicht, ist die
Aufrufstelle im async-Kontext ein Absturz — kein Fehlerwert.**

**Keine neuen Crates in `Cargo.lock`, nachgeprüft:** `secrecy` war über
`harw-lens-embed` aufgelöst, `tokio` nur von `[dev-dependencies]` nach
`[dependencies]` verschoben, mit auf `["rt"]` reduziertem Merkmalsbedarf.

*Was `Confidential` weiterhin nicht hat:* einen produktionsreifen Einbetter.
`lens.ask` fragt den Katalog ausschließlich für `EmbeddingRole::Query`,
`route()`s fail-closed-Filter und `RemoteEmbedder::locality()` bleiben
unverändert scharf, und ein lokales Modell existiert nicht im Baum (K71: 171
zusätzliche Crates und ein C-Übersetzer). **Steht so in der Moduldoku**, statt
verdeckt zu werden.

*Nachtrag aus der zentralen Verifikation:* der Knoten hatte
`&Box<dyn Embedder>` für `&dyn Embedder` gehalten und die Koerzierung als
geprüft gemeldet. `federated_query` nimmt `&dyn Embedder`, und
`Box<dyn Embedder>` implementiert `Embedder` nicht — fünf Stellen, behoben mit
`embedder.as_ref()`. **Das ist genau die Klasse Fehler, für die die zentrale,
sequenzielle Verifikation existiert:** eine von Hand nachgelesene Signatur
ersetzt keinen Übersetzerlauf, und die Auflage „kein `cargo` im Agenten"
verlagert diese Prüfung bewusst hierher, statt sie wegzulassen.

### K79 · Die Gates laufen seit AW0-00 nicht mehr — und mit ihnen vier weitere Produktionspfade

**Der schwerste Befund der Abschlussanalyse, und er ist keine Lücke im
Programm, sondern eine Folge davon.**

```
$ cargo run -q -p xtask -- gates
xtask gates: Workspace-Graph nicht lesbar: TOML parse error at line 3, column 1
  |
3 | version.workspace = true
  | ^^^^^^^
invalid type: map, expected a string
```

**Ursache**, `harw-code-graph/src/workspace.rs`:

```rust
struct RawPackageSection {
    name: String,
    #[serde(default)]
    version: Option<String>,
}
```

AW0-00 hat alle 95 Member-Manifeste auf `version.workspace = true`
vereinheitlicht — **eine Tabelle, keine Zeichenkette**. `WorkspaceGraph::load`
scheitert seither am ersten Member.

**Der Schaden ist nicht auf die Gates beschränkt.** Alle Aufrufer außerhalb
von Tests:

| Aufrufer | Folge |
|---|---|
| `xtask/src/gate_privileges.rs:593`, `gate_edges.rs:160` | **Alle neun Gates blind** — verbotene Kanten, Privilegienbudget, Warden-Abhängigkeiten, C-Build, Schreibbereichs-Disjunktheit. |
| `xtask/src/webui.rs:179` | Der Typgenerator für die Control Plane. |
| `harw-ops/src/analyze.rs:887` | Die `/analyze`-Operation scheitert auf diesem Workspace. |
| `harw-tool-deps/src/graph_tool.rs:216` | Das Abhängigkeitsgraph-Werkzeug, das Agenten benutzen. |
| **`harw-dod-workspace/src/inventory.rs:233`** | **Der Drift-Sensor.** |

**Die letzte Zeile ist die schlimmste.** Der Sensor schreibt

```rust
WorkspaceGraph::load(&canonical_root).map_err(|_| WorkspaceError::MalformedSource)?;
```

Er bildet **jeden** Ladefehler auf `MalformedSource` ab. Auf diesem Workspace
meldet er damit „Quelle fehlerhaft" — eine Aussage über den **überwachten
Baum**, obwohl es eine über den **Parser** ist. Ein Sicherheitssensor, der
strukturell nie eine Drift findet und dabei aussieht, als hätte er nachgesehen.

**Warum es niemand gemerkt hat — und das ist die eigentliche Lehre.**
`write_crate` im Testmodul von `workspace.rs` schreibt

```
[package]\nname = "{name}"\nversion = "0.1.0"\n
```

also die **Zeichenketten-Form**. Jeder Test konstruiert sich einen Workspace,
den es im Repo **seit AW0-00 nicht mehr gibt**. Die Tests sind grün und haben
die Wirklichkeit nie berührt.

**Das ist die fünfte Fundstelle des Musters aus K43**, in einer neuen Form:
nicht der Vergleichswert stammt aus derselben Quelle wie der geprüfte Wert,
sondern die **Eingabe** stammt aus derselben Annahme wie der geprüfte Code.
Ein Parser, der nur gegen selbst erzeugte Manifeste getestet wird, prüft seine
eigene Erwartung.

**Und ein zweiter Grund, warum es so lange unbemerkt blieb:** das `Makefile`
ruft `xtask gates` **nirgends** auf. Seine Ziele sind `clippy-tests`,
`clippy`, `tests`, `fmt`, `check`, `build`, `install`, `service`. Der Plan
schreibt in Entscheidung Nr. 2: *„Stub in AW0-00, `Makefile` ruft es."* **Der
zweite Halbsatz ist nie eingelöst worden.**

**Es gilt:** der Parser liest alle drei gültigen `version`-Formen, eine
unbekannte vierte liefert einen **Fehler statt eines leeren Werts**, mindestens
ein Test lädt den **echten** Workspace statt eines konstruierten, und das
`Makefile` ruft die Gates auf. Der `map_err(|_| MalformedSource)` in
`harw-dod-workspace` ist gemeldet, nicht angefasst — ein Ladefehler ist von
einer Fehlform der Quelle zu unterscheiden.

*Der allgemeine Punkt, und er wiegt schwerer als der Fall:* die Zahlen aus
K50 — 87 Knoten, 137 Schreibbereiche, ein Verstoß — **stammen aus einem Lauf,
der stattgefunden hat, als der Graph noch las.** Jede Aussage über
Disjunktheit, Privilegienbudget und verbotene Kanten seither ist **unbelegt**.
Sie war nie falsch; sie war ungeprüft, und das sah genauso aus.

### K80 · Die Abschlussanalyse: was geplant war, was steht, und was nie einen Aufrufer bekam

Sechs unabhängige Prüfungen, je ein Teilsystem, bottom-up (existiert das
Symbol, hat es einen Konsumenten, führt ein Produktionspfad hin) und seitwärts
(halten die Cross-Crate-Verträge und die Invarianten). Dazu eine mechanische
Erhebung der Wellentabellen.

#### Was der Plan behauptet und was zutrifft

| Kennzahl | Plan | Nachgemessen |
|---|---|---|
| Knoten in den Wellentabellen | 93 | **92** (86 Zeilen, davon eine Bereichszeile für sieben Sensoren) |
| Crates im Workspace | 95 | **95** |
| Tests | — | **6616 grün** |

Die Differenz von einem Knoten ist Buchhaltung, kein Befund.

#### Die Verträge halten — durchgehend

Stichprobenartig gegen `docs/aw-contract-master.md` geprüft, **keine
Signaturabweichung gefunden**:

- **`TelemetrySink`**: alle drei Driftpunkte (Receiver `&self`, Rückgabe `()`,
  synchron) exakt wie fixiert.
- **`ChunkDigest`** ist ein Newtype über `ContentDigest` — die offene Frage aus
  Abschnitt B ist entschieden und umgesetzt.
- **`Fragment`** trägt alle sieben Felder, **`FragmentOrigin`** statt
  `Provenance` (K15).
- **`trait Sensor`** liegt in `harw-dod-signals` (K24), **`Finding<S>`
  vollständig in `harw-dod-rules`** (K25) — `harw-dod-signals` enthält keinen
  `Finding`-Typ.
- **S1 hält**: `authorize` ist `pub(crate)`, und **zwei** `compile_fail`-
  Doctests belegen es — einer über die Sichtbarkeit, einer über die Fassade,
  die `Action`/`Authorized` nicht einmal benennt.
- **K5** (`Freeze` über `(CgroupId, FindingId, frozen_at)`), **K49**
  (`sync_parent_directory` an genau einer Stelle), **K53** (Warden bei **12**
  direkten Laufzeit-Abhängigkeiten, `aya` und `nix` liegen **nicht** in seiner
  Hülle), **C7** (Geschwister kennen einander nicht) — alle bestätigt.
- **Kein C-Build in der Warden-Hülle.** `aya` und `landlock` haben
  `build = false`; `rustix`' Bauskript ist reine Feature-Erkennung ohne
  `links`.

#### Die Kette, die niemand betritt

**Das ist der Befund, der über allem steht.** Drei Teilsysteme sind vollständig
gebaut, typsicher, getestet — und haben **keinen Produktionsaufrufer**:

| Kette | Bricht bei | Folge |
|---|---|---|
| **Sicherheit** | `run_rules` und `triage` haben **workspace-weit keinen Aufrufer außerhalb von Tests**; `harw-sentinel` hängt nicht einmal an `harw-dod-rules` | Alles danach ist unerreichbar: `Finding<Triaged>`, `Action::authorize`, `WardenActionRequest`. **`harw-warden` ist ein fertiger Empfänger ohne Sender.** |
| **Kontext** | `AgentSession::with_context_program` hat **keinen Aufrufer außerhalb von `#[cfg(test)]`**; `ModelRequest::with_context_budget` ruft die **alte** `assemble` | Jede reale Sitzung hat `context_program() == None`. `render_trust_blocks_with_detail` läuft nie — **die Zwei-Block-Trennung aus AW4-01 ist in Produktion nicht aktiv**, und der Nullzähler `trust_block_violation` ist von keinem Eintrittspunkt erreichbar. |
| **Lens** | `planner.toml`s `[tools].admitted` listet **`lens.ask` nicht** | Die einzige Rolle mit Zugriff auf `RegistryProfile::Planning` sieht das Werkzeug im System-Prompt-Inventar und **darf es nicht aufrufen** — `SessionActivation` ist deny-by-default. |

**Elf Crates und fünf Wellen Retrieval, eine fehlende Zeile in einer TOML.**

#### Warum die Prüfungen das nicht gefunden haben

In allen drei Fällen dasselbe Muster, und es ist das aus K43:

- **Lens:** `approval_allow_list_covers_every_builtin_role_tool` prüft
  `RegistryProfile::tool_names()` gegen `AUTO_APPROVED_TOOLS` — **beide aus
  `profile.rs`**. Die Prüfung kann eine Abweichung der TOML-Seite strukturell
  nicht sehen. Der Gegentest prüft `admitted` nur gegen eine **Verbotsliste**,
  nie gegen die Positivliste des Profils.
- **Kontext:** `harw-core/tests/detail_mode_references.rs` verdrahtet
  `ContextProgram`, `Assembly` und `ContextLoadExecutor` **von Hand** und ruft
  `drive_turn` nie auf. Er belegt, dass die Teile zusammenpassen, nicht dass
  sie zusammengesetzt werden.
- **Lens-Werkzeug:** `test_agent_reaches_lens_hits_through_the_tool` ruft den
  Executor direkt — ohne `SessionActivation`, ohne Profil, ohne Agenten-IR.
  **Der Test heißt „ein Agent erreicht Lens" und beweist genau das nicht.**

#### Die Gates: drei von neun, und keines wird aufgerufen

| Gate | Stand |
|---|---|
| verbotene Kanten, Privilegienbudget, Schreibbereichs-Disjunktheit | **existieren, implementiert, `checked > 0`** |
| Warden-Abhängigkeitszahl, kein C-Build im Warden-Teilbaum, keine Route ohne `OperationMeta`, Tier-Ablehnungsmatrix, Harness-Vollständigkeit je Sensor, OTel-Adapter-Isolation | **existieren nicht, nicht einmal als Gerüst** |

Dazu K79: die drei vorhandenen liefen seit AW0-00 gar nicht, und das `Makefile`
ruft sie nicht auf. **Sechs der neun Gates des Verifikationsplans sind nie
gebaut worden** — der Plan führt sie als Bestandteil der Abnahme.

#### Was die Prüfungen bestätigt haben

Nicht alles ist offen. Ausdrücklich in Ordnung, jeweils mit Beleg:

- **Der Sentinel fährt zehn Sensoren** und begründet je Ausnahme am Ort, warum
  ein Sensor in ein privilegiertes Binary gehört.
- **Kein Sensor mintet ein `Finding`** — elf Crates geprüft, null Treffer.
- **Der Glob im Sensor-Makro ist ein Laufzeit-Suffix** (K42 hält).
- **Die Telemetrie-Trennung ist strukturell**, nicht bloß getestet: die
  Kompositionsstelle kennt keinen Weg, eine `ExportApproval` herzustellen, und
  ein Test belegt, dass `security.finding_total` den Zusatzsink mit **null**
  Aufrufen erreicht.
- **Null erschöpfende `match`-Arme** auf `Surface` brechen an einer neuen
  Variante (K51 bestätigt), **16** `Surface::Web`-Deklarationen, **keine**
  Pfadkollision.
- **`ApprovalActor` kommt strukturell nur aus `SO_PEERCRED`**, die
  Genehmiger-Tabelle hat keinen Vorgabewert, eine zweite Bestätigung wird
  abgewiesen.
- **Vier systemd-Units, vier Privilegienklassen**, deckungsgleich mit
  `BinaryBudget`.

#### Weitere offene Punkte, verortet

| Offen | Ort |
|---|---|
| Die Bestätigungsfläche liefert immer `NotAvailable` | `harw-cli/src/web.rs::build_web_op_context` verwirft den `peer` und legt weder `PeerCredentials` noch einen `ApprovalActorResolver` in die `ServiceMap` |
| `WEB_ROUTES` ist `[] as const` bei 16 realen Routen | `webui/lib/generated/operations.ts`; der Generator wurde nie erneut ausgeführt |
| Die Oberfläche liest das `trust`-Feld nicht | `DataBlock.tsx`, mit einem Kommentar, der inzwischen falsch ist |
| Die neun Golden Renders werden nie ausgeführt | `agents/context-programs/golden/`; `TESTS.txt` sagt es selbst |
| AW6-03 (vier Triage-Rollen) und der Security-Clan existieren nicht | `[[clans]]` ist in `security.toml` auskommentiert, `[workers].allowed = []` |
| `audit_chain_break` ist ein internes `AtomicU64`, keine benannte Metrik | `harw-secrets/src/audit/mirror.rs` |
| `chunk_plain` hat weiterhin null Konsumenten | dokumentiert im Code |
| `validate_patch` hat keine Eingabe | nirgends entsteht ein `UnifiedDiff` |
| Doku-Drift: `pack.rs` zitiert die durch K55 **zurückgenommene** Abnahme | `harw-lens-rank/src/pack.rs:5-7` |
| Der Angreifer-Fixture-Test baut die Decke von Hand nach | die TOML-Datei ist Dokumentation ohne ausgeführten Pfad |

#### Das Urteil

**Der Plan wurde umgesetzt wie beschrieben — die Bauteile stimmen, die
Verträge halten, die Invarianten sind eingehalten, und die Korrekturen K1–K79
sind nachweisbar im Code.** Was fehlt, ist nicht Bausubstanz, sondern
**Anschluss**: drei große Ketten enden kurz vor ihrem ersten echten Aufrufer,
und die Prüfungen, die das hätten fangen sollen, vergleichen jeweils zwei
Werte aus derselben Quelle.

Das ist exakt die Diagnose, die dieses Programm sich selbst gestellt hat —
`[[harwness-gruene-gates-beweisen-nichts]]`. Sie trifft am Ende auf das
Programm selbst zu.

### K81 · Die Gates laufen wieder — und der „Zyklus" war eine Kante auf ein Nicht-Member

Nach der Reparatur des `version`-Parsers (K79) scheiterte der Graph an der
nächsten Stufe: *„Abhängigkeitszyklus erkannt: `[harw-secrets, harw-channel,
harw-channel-telegram, harw-channel-telegram-transport, harw-cli]`"*.

**Es war keiner.** `harw-secrets/Cargo.toml` trägt

```toml
crypt_guard = { version = "3.0.2", path = "../../crypt_guard" }
```

— ein legitimer Pfad auf ein Nachbar-Repo **zwei Verzeichnisse über dem
Workspace**, kein Member. `classify_dependencies` hielt aber **jede**
Abhängigkeit mit einem `path`-Feld für workspace-intern
(`has_path_field || member_names.contains(dep_name)`). `crypt_guard` landete so
in `harw-secrets.deps`, tauchte nie als Knoten auf, und Kahns Algorithmus
konnte den Namen nie auflösen — jeder transitiv abhängige Knoten blieb mit ihm
in der Restmenge hängen.

**Zwei Korrekturen:**

1. **Zugehörigkeit entscheidet die Mitgliedschaft, nicht das `path`-Feld.**
   Geprüft: kein Manifest benutzt eine `package = "..."`-Umbenennung auf einen
   Pfad-Dependency, es bricht also keine echte interne Kante.
2. **Eine hängende Kante heißt jetzt `MemberMissing`, nicht `CycleDetected`.**
   `compute_levels` prüft **vor** dem Lauf, ob jede genannte interne
   Abhängigkeit als Knoten existiert. Die passende Fehlervariante existierte
   bereits; `error.rs` blieb unangetastet.

**Der zweite Punkt ist der wichtigere.** Eine Fehlermeldung, die „Zyklus" sagt,
wo „unbekannte Kante" gemeint ist, schickt den nächsten Leser auf eine Suche
ins Leere — und **ich bin genau darauf hereingefallen**, bis ich die
`Cargo.toml` von `harw-secrets` gelesen habe. Dieselbe Klasse wie K40, wo eine
Sicherheitsregel ihre Einstufung aus Prosa rekonstruierte: das Werkzeug meldete
eine Diagnose, die es nicht belegen konnte.

#### Der erste Gate-Lauf seit AW0-00

```
edges:       grün (495 geprüft)
privileges:  grün (4 geprüft)
writescopes: grün (139 geprüft)
```

**Alle drei grün, alle drei mit echten Kandidaten** — keines meldet grün mit
`checked: 0`, die Verwechslung, gegen die die Zahl eingeführt wurde.

Der Vergleich zum K50-Stand: **139 statt 137** geprüfte Schreibbereiche (der
Plan ist seither um Zeilen gewachsen), und **null Verstöße statt einem** — die
damals gemeldete Überlappung zwischen AW0-07 und AW6-02 wurde durch die
nachgetragene Kante geschlossen und bleibt geschlossen.

Eine Zelle wird ausdrücklich übersprungen und sagt es:
`AW0-00`s *„alle neuen Crate-Stubs"* ist Prosa ohne Backtick-Pfad. **Das Gate
meldet das, statt still weiterzuzählen** — es ist eine unauswertbare Angabe,
kein grüner Haken.

**Was das für die Bilanz bedeutet:** die Aussagen über verbotene Kanten,
Privilegienbudget und Schreibbereichs-Disjunktheit sind **zum ersten Mal seit
AW0-00 wieder belegt** statt bloß behauptet. Sie waren die ganze Zeit richtig —
das war Glück, nicht Nachweis.

### K82 · Bruch 1 ist geschlossen — und hat einen HTTP-Stack in ein Sicherheitsbinary gezogen

`harw-dod-rules::run_rules` läuft jetzt im Poll-Loop von `harw-sentinel`,
unmittelbar nach `Sentinel::freeze(now)` — **mit demselben `now`, ohne zweiten
Uhrenzugriff**. Die Befunde gehen über denselben `TelemetrySink`, den der
Sentinel ohnehin bedient; **kein zweiter Ausgabeweg**.

Damit hat `run_rules` zum ersten Mal einen Produktionsaufrufer. Vorher lag
jede Fundstelle in einer Definition, einem Doctest oder einem
`#[cfg(test)]`-Modul.

**Und dann hat der Knoten nachgezählt, statt es dabei zu belassen.** Die Kante
bringt **21 neue Einträge** in den Abhängigkeitsbaum des Sentinels — darunter,
transitiv über `harw-knowledge → harw-model-catalog`, **`reqwest` und
`tokio`**:

> **Ein voller async-HTTP-Stack erreicht dieses unprivilegierte Binary,
> obwohl keine Regel je das Netz berührt.**

**Er hat es gemeldet statt die Kante zu entfernen** — das wäre die Rückkehr in
den unverdrahteten Zustand gewesen, also der Tausch eines Befundes gegen einen
schlechteren.

#### Die Ursache ist zwei Zeilen lang

```rust
// harw-dod-rules/src/baseline.rs:62-63
pub use harw_knowledge::memory::palace::PalaceStatus;
pub use harw_knowledge::security::Baseline;
```

**Zwei Typreexporte ziehen eine ganze Wissensbasis samt HTTP-Client.**

**Das ist wörtlich das Muster aus K3**, nur in die andere Richtung. Dort wurde
entschieden, `Confidence` nach `harw-types` zu ziehen statt einen Reexport zu
bauen, mit der Begründung: *„das zöge die ganze Memory-Crate für ein Enum
herein."* Hier ist genau das passiert — und niemand hat es bemerkt, weil die
Kante nie in einem Binary landete. **Erst die Verdrahtung hat den Preis
sichtbar gemacht.**

*Der allgemeine Punkt:* eine Abhängigkeit, die nur in Bibliotheken hängt,
kostet nichts Sichtbares. Ihr Preis entsteht an dem Tag, an dem ein Binary sie
erbt — und dann ist sie alt, begründet und schwer zu entfernen. **Das
Privilegienbudget je Binary ist genau dafür da, und es hat hier zum ersten Mal
etwas gefunden.**

Als eigener Knoten vergeben: das Vokabular nach unten ziehen oder die Baseline
über einen hereingereichten Trait beschaffen — mit der Messlatte, dass
`harw-sentinel` danach **weder `reqwest` noch `tokio`** in seiner Hülle hat und
die Regelauswertung unverändert läuft.

#### Bruch 2 bleibt, und ist jetzt genauer benannt

`Finding<RuleChecked>` wird **nicht triagiert** — `triage` hat weiterhin
keinen Produktionsaufrufer, und `harw-warden` hat keinen Sender. Die
Einschätzung des Knotens, und sie deckt sich mit der Doku von
`harw_dod_rules::engine`:

> Der fehlende Knoten ist ein **stärker privilegierter Prozess**, der
> gemeldete Befunde zurückliest, `triage` aufruft und bei `Verdict::Confirmed`
> die Anfrage an `harw-warden` sendet.

**Das ist eine Architekturentscheidung, keine Verdrahtung** — und sie gehört
nicht in einen unprivilegierten Sensor. Der Sentinel beobachtet; er setzt
nichts durch, und er darf es nicht.

### K83 · Die Montage v2 ist in Produktion — und der Blocker war ein anderer als gedacht

Der Plan führte als Ursache: *„kein `ContextProvider` erzeugt
`harw_context::Fragment`."* **Das stimmte nicht mehr.** Der Knoten hat
nachgesehen und gefunden:

> `gather_context` rief `contribute_v2` bereits und **bekam echte
> `harw_context::Fragment`-Werte** — und verwarf die reicheren Daten sofort
> wieder, indem es auf `ContextFragment` zurückprojizierte. Grund:
> `ModelRequest::with_context_budget` nahm nur den alten Typ entgegen.
> **Diese Hin-und-Rück-Wandlung war der Blocker, nicht ein fehlender
> Erzeuger.**

Der Fragment-Knoten (K75) hatte den Erzeuger geschaffen; die Rückwandlung
zwei Zeilen später hat ihn wieder unwirksam gemacht. **Zwei Knoten, jeder für
sich korrekt, und dazwischen ging die Information verloren.**

**Gebaut:** `ModelRequest::with_context_program(...)` nimmt v2-Fragmente, ein
`ContextProgram` und eine geschnittene `ContextCeiling` und fährt
`Assembly::gather → admit → budget → render`. Der `DetailMode` je Sektion wird
aus `ContextProgram::section_detail()` gelesen — **der erste
Produktionskonsument dieser Daten** — und
`render_trust_blocks_with_detail` aufgerufen.

**Die Prüfung, die den Unterschied macht:** der Datenblock landet als
`ContextFragment` in `Self::context` — **demselben Feld, das
`harw-provider-http` bereits für den Wire-Aufbau liest**. Der Knoten hat das
nachgesehen, statt ein neues Feld anzulegen, das niemand liest. **Die Trennung
erreicht damit den API-Aufruf und ist kein inertes Feld.**

Ein `must_include`-Verstoß bricht den Turn ab, über
`ModelError::ContextAssembly` und die bestehende `CoreError::Model`-Wandlung —
**gekürzt wird nicht.**

**Der Stand danach:**
- Die **AW4-01-Trennung läuft in Produktion** für jede Sitzung, die ein
  Programm **und** eine geschnittene Decke trägt.
- **`TRUST_BLOCK_VIOLATION` ist von `drive_turn` aus erreichbar**, nicht mehr
  nur aus Unit-Tests.
- Eine Sitzung ohne beides rendert **byte-identisch** wie zuvor — belegt durch
  einen Test, der die Ausgabe gegen `with_context_budget` vergleicht.

**Offen und benannt:** `ContextAssemblyV2::spent_per_section` hat weiterhin
keinen Produktionsaufrufer. `seed_context_load_ledger` läuft **einmal vor der
Schleife**, bevor eine Montage existiert; echte Sektionskosten dorthin zu
speisen verlangt eine Umstellung der Aufrufreihenfolge in `drive_turn`. **Als
Folgeknoten dokumentiert statt halb gebaut.**

### K84 · Der HTTP-Stack ist wieder draußen — das Vokabular ist nach unten gewandert

Die Kante `harw-dod-rules → harw-knowledge` bestand für **zwei Typreexporte**
und brachte über `harw-model-catalog` einen vollständigen async-HTTP-Stack in
ein unprivilegiertes Sicherheitsbinary (K82).

**Gewählt: das Vokabular wandert nach unten.** `Baseline`, `PalaceStatus` und
`BaselineError` sind jetzt in `harw-dod-rules` **lokal** definiert — sie sind
reine Daten und brauchen keine Wissensbasis. `harw-knowledge` bekommt eine
Methode `Baseline::to_rule_baseline()` und **konvertiert oben**, statt unten
reexportiert zu werden.

**Das ist wörtlich das K3-Muster mit vertauschten Rollen.** Dort galt: der Typ
zieht nach unten, beide reexportieren, *„ein Reexport zöge die ganze
Memory-Crate für ein Enum herein."*

**Ein Detail lohnt sich zu merken:** `Baseline::new` nimmt den Bezeichner als
`impl Display`. Damit erfüllt `harw_knowledge::artifact::ArtifactId` die
Signatur, **ohne dass die Produktionssignatur `harw-knowledge` je beim Namen
nennt.** Die Kante verschwindet, die Benutzbarkeit bleibt.

Die beiden anderen Wege wurden mit Begründung verworfen: ein
`BaselineSource`-Trait hätte bei **einer** echten Quelle die Kante nur hinter
einer Abstraktion versteckt; ein Feature-Schalter hätte die Entscheidung ins
Standardprofil verschoben, wo der Sentinel aktiv daran hätte denken müssen.

#### Nachgezählt, was aus den 21 Einträgen wurde

**Übrig:** `harw-sandbox` (für `EgressFlowRule`, ohnehin schon direkte
Sentinel-Kante) und `harw-research` (für `epistemic_confidence_for`).

**Verschwunden:** `harw-knowledge`, `harw-model-catalog`, `harw-agent-dsl`,
`harw-config`, `harw-context`, `harw-job-runtime`, `harw-lens-types`,
`harw-plan`, `harw-session-store`, dazu extern `fs4`, `ipnet`, `secrecy`,
`serde_norway`, `tempfile`, `time`, **`tokio`** und **`reqwest`**.

#### Die Kantenrichtung, geprüft

`harw-knowledge` hängt jetzt **produktiv** an `harw-dod-rules`;
`harw-dod-rules` hängt an `harw-knowledge` **nur unter
`[dev-dependencies]`** — für einen unveränderten Doctest, der einen
`ArtifactId` konstruiert. **Cargo unterstützt Dev-Dependency-Zyklen
ausdrücklich**; sie fließen nie in den produktiven Graphen. Nachgeprüft, dass
keine der verbleibenden Produktionskanten von `harw-dod-rules`
(`harw-sandbox`, `harw-types`, `harw-research`, `harw-code-graph`,
`harw-dod-signals`) auf `harw-knowledge` zurückzeigt.

*Was daran über den Fall hinaus zählt:* die Regelkette ist **unverändert
geschlossen** — Feldnamen, Methodensignaturen und Typnamen sind gleich
geblieben, `harw-sentinel` wurde nicht angefasst. **Eine Abhängigkeit zu
schneiden, ohne den Konsumenten anzufassen, ist der Beleg dafür, dass sie
Vokabular war und nicht Verhalten.**

### K85 · Die Auditkette hat einen Zähler, einen Scheduler — und beinahe die falsche Kette geprüft

Drei Knoten haben nacheinander an derselben Kette gearbeitet, und jeder hat den
nächsten Befund freigelegt.

**Erstens:** `audit_chain_break` war ein internes `AtomicU64`, **keine benannte
Metrik**. Ein Zähler, den niemand abfragen kann, ist eine Variable — und ein
Nullzähler ist gerade deshalb einer, weil jemand nachsieht, dass er null ist.
Er heißt jetzt so und trägt einen `invariant()`-Text.

**Zweitens:** `verify_and_mirror` hatte **workspace-weit keinen Aufrufer**
außerhalb von Tests; `TelegramMirrorTransport` war korrekt gebaut und wurde
von niemandem konstruiert. **Die Kette brach am Scheduling-Punkt.** Jetzt läuft
sie alle 15 Minuten als vierter `select!`-Zweig des Gateways, über
`spawn_blocking`.

Zwei Entscheidungen dieses Knotens verdienen es, festgehalten zu werden:

- **`NoMirrorTransport`** meldet jeden Sendeversuch **ehrlich als abgelehnt**,
  statt einen nie stattgefundenen Versand als Erfolg zu verbuchen. Die
  Alternative — Chat-Zugangsdaten für den Spiegel zweckzuentfremden — hätte
  den Gedanken eines **zweiten unabhängigen Kanals** unterlaufen.
- **`spawn_blocking` wurde angewandt, obwohl geprüft war, dass dieser Pfad
  keine verschachtelte Runtime aufbaut.** Nach drei Fundstellen derselben
  Falle ist das die richtige Vorsicht.

**Drittens, und das ist der eigentliche Befund:** die geprüfte Kette war eine
**gateway-eigene**, nicht die des konfigurierten Geheimnisspeichers — dessen
`AuditLog` wird in `open_configured_secret_resolver` nur **transient**
geöffnet und überlebt keinen Tick.

**Eine Manipulationsprüfung auf einer Kette, die niemand sonst beschreibt,
prüft nichts. Sie ist grün, weil sie leer ist — und das sieht genauso aus wie
„unversehrt".** Der Knoten hat das gemeldet, obwohl sein Auftrag formal
erfüllt war; die Umlenkung ist als eigener Knoten vergeben.

*Nebenbefund, dieselbe Falle wie beim `SpawnContext` (K70):* das Intervall
sollte ein `cli.rs`-Schalter werden. Ein neues Feld auf `TelemetryArgs` oder
`Command::Gateway` hätte **exhaustive Struct-Literale** in zwei fremden
Dateien gebrochen. **Ein Struct mit `pub`-Feldern und Literal-Konstruktion an
mehreren Stellen ist gegen additive Erweiterung verschlossen, obwohl es offen
aussieht.**

### K86 · Die zweite Rollenliste — und warum sie bleiben musste

AW6-00 hat `include_str!`-Listen durch eine Verzeichniskonvention ersetzt,
damit gilt: *eine Definition hinzufügen ist eine Datei anlegen.*
**`role_names::ALL` war die Liste, die dabei übersehen wurde.**

Die Folge war ernst: die vier neuen Triage-Rollen existierten, wurden
**gefunden** — und **nie gesenkt**, weil `builtin_agent_definitions` über
`ALL` filtert. **Der Verbotstest für `fs.write`/`shell.exec` erreichte sie
nicht.** Eine Rolle, die durch eine Sicherheitsprüfung fällt, weil eine Liste
sie nicht kennt, ist schlimmer als eine, die es nicht gibt.

**Die Liste konnte nicht verschwinden, und der Grund ist belastbar:**
`ALL` ist `pub const &'static [&'static str]` und wird an vier Stellen
außerhalb der Crate **direkt als Wert** benutzt (`for role in role_names::ALL`,
`.contains(…)`, `.len()`). Eine zur Laufzeit abgeleitete Liste wäre ein
`LazyLock<Vec<…>>` — nicht `Copy`, kein `IntoIterator` auf dem Platzausdruck.
Der Umbau hätte vier fremde Dateien gebrochen.

**Es gilt deshalb Weg 2, aber vollständig:** die vier Namen sind eingetragen,
**und** ein Test vergleicht `role_names::ALL` gegen die **verzeichnisbasierte
Entdeckung** — zwei unabhängige Quellen, abzüglich zweier dokumentierter
Ausnahmen. **Eine Liste ohne Prüfung ist genau das, was hier schiefging.**

*Und eine Entscheidung, die der Deckungstest sofort erzwungen hat:* die
Triage-Rollen bekommen ein neues Profil **`NoTools`** statt `ReadOnlyExplore`.
Deren zehn beworbene Werkzeuge hätten gegen `admitted = []` gestanden —
**dieselbe Fehlerklasse wie `lens.ask`, nur umgekehrt.** Mehr im Inventar als
erlaubt ist so falsch wie weniger erlaubt als beworben, und der Test prüft
seit K80 beide Richtungen.

**Eine dritte Liste wurde gefunden und bewusst liegen gelassen:**
`[workers].allowed` in `security.toml` — die Familienmitgliedschaft, die die
Datei selbst als offenen Befund führt.

### K87 · Die Gates messen zum ersten Mal richtig — und was sie dabei gefunden haben

Alle fünf Gates laufen. Ergebnis des ersten belastbaren Laufs:

```
edges:                    grün (498 geprüft)
privileges:               grün (4 geprüft)
writescopes:              grün (139 geprüft)
warden-dependency-budget: grün (46 geprüft)
warden-no-c-build:        1 Verstoß bei 46 geprüften Kandidaten
```

**Drei Befunde auf dem Weg dorthin, und jeder war ein Fehler im Messwerkzeug,
nicht im Gemessenen:**

1. **`Cargo.lock` sagt nicht, was gebaut wird.** Es enthält die Auflösung
   **aller** Features, auch der nie aktivierten. `defmt` stand im
   `jiff`-Block, weil `jiff` das Feature *anbietet* — keine der rund fünfzig
   `jiff`-Stellen fordert es an. Das Gate zählte Code mit, der nie im Binary
   landet, und meldete ihn als C-Build-Verstoß.
2. **Der Auflöser übersah die Kurzform.** `[workspace.dependencies]` erlaubt
   `name = "1.2.3"` **und** die Tabellenform; die Wurzel benutzt beide. Der
   Parser legte bei der Kurzform **gar keinen** Eintrag an — nicht einmal
   einen voreingestellten. Fünf Kanten blieben unaufgelöst, und mit ihnen
   **ganze Teilhüllen**.
3. **Die Zahl war eine Handrechnung.** Erwartet waren 63, gemessen wurden
   **46**. Die Konstante steht jetzt auf dem gemessenen Wert — eine
   Sperrklinke ist nur eine, wenn sie am Istwert klebt. Eine Differenz von 17
   ist kein Puffer, sondern war eine Fehlmessung.

**Was echt bleibt und stehen wird:** `blake3` deklariert seine
`cc`-Build-Dependency **ohne `optional = true`** — die Kante besteht für jede
Feature-Kombination, und das Merkmal `pure` schaltet nur die SIMD-Erzeugung
ab. **Ein echter Verstoß gegen „kein C-Build im Warden-Teilbaum", seit
Programmbeginn, mit den erlaubten Mitteln nicht behebbar** (ein
Versionswechsel wäre nötig). Er steht als bewusst offener Punkt, nicht
wegdefiniert.

*Und ein Befund, den das Privilegien-Gate geliefert hat:* die Verdrahtung der
Regelauswertung brachte `harw-dod-rules` und `harw-research` in die
Sentinel-Hülle, für die keine Fähigkeitszuordnung hinterlegt war. **Das Gate
hat sie als Verstoß gemeldet statt sie durchzuwinken** — genau die Regel, die
es wertvoll macht. Beide sind nachgeprüft unprivilegiert; die ganze Hülle (23
Crates) wurde gegengelesen, keine weitere fehlt.

### K88 · Die Wurzeldecke führte eine einzige Sektion — und niemand hat es gemerkt

Die neue Prüfung aus K85 (ein mitgebrachtes Programm darf die geschnittene
Decke nicht erweitern) hat beim ersten Testlauf **jede** eingebaute Rolle
abgelehnt:

```
role 'explorer' must be admitted: child context program escalation rejected:
declared section 'task.objective' is not part of the child's context ceiling
```

**Nachgemessen:** `local_root_context_ceiling()` führte **genau eine**
Sektion, `history.tail`, mit einem Kommentar, der das selbst als *bekannte
Lücke* deklarierte — *„nur `HISTORY_TAIL_SECTION` ist über die Crate-Grenzen
hinweg als öffentliche Konstante erreichbar."* Eine
**Erreichbarkeits-Notlösung**, keine Sicherheitsentscheidung.

Dem gegenüber deklariert `worker-base.toml` drei Sektionen
(`task.objective`, `task.read_scope`, `new.trigger_return`), und ein
bestehender Test belegt, dass **jede** eingebaute Rolle genau diese trägt. Die
Schnittmenge war leer.

**Die Prüfung hatte recht, die Decke war falsch** — und sie war folgenlos
falsch, solange keine Sitzung ein Programm trug. **Das ist die Klasse Fehler,
die erst beim Anschließen sichtbar wird:** eine Notlösung, die niemanden
störte, weil der Pfad, der sie berührt hätte, nicht existierte.

Korrigiert an der **einen** Stelle, an der die Wurzeldecke entsteht
(`root_context.rs`), nicht an der Aufrufstelle — sonst wären die drei anderen
Wurzel-Einstiege im Fehler stehen geblieben. Die Decke bleibt eine echte
Obergrenze: vier Sektionen, nicht „alle", und ein neuer Test hält fest, dass
`credential.token`, `secret.vault`, `full_parent_transcript`,
`sibling_transcripts`, `plan.current` und `web.fetch_allowlist`
**nicht** darin stehen.

*Nebenbefund, der die Bilanz betrifft:* die **neun Kontextprogramme** unter
`agents/context-programs/` sind an **keine** eingebaute Rolle gebunden — die
Rollen laufen über die `[context]`-Tabelle in `worker-base.toml`, nicht über
`harwness.context.<name>@<v>`. `task.objective` stammt aus `worker-base.toml`.
**Die Programmbibliothek hat weiterhin keinen Konsumenten**, auch mit den
frisch gebauten Golden-Tests.

### K89 · Vier Testfehlschläge, vier verschiedene echte Ursachen

Der erste vollständige Testlauf nach der Verdrahtungswelle lieferte in vier
Runden je **einen** Fehlschlag — und keiner war ein Artefakt:

| Runde | Fehlschlag | Ursache |
|---|---|---|
| 1 | `explorer` abgelehnt | Die Wurzeldecke führte **eine** Sektion (K88). |
| 2 | `security-egress-triage` abgelehnt | `role_names::ALL` beantwortet **zwei** Fragen. |
| 3 | „keine Netz-Crate" | Eine **Volltextsuche** traf einen Kommentar. |
| 4 | Nullzähler bei 1 statt 0 | Prozessweiter Zähler **ohne Sperre**. |

**Runde 3 ist die dritte Fundstelle eines dokumentierten Musters** — und die
mit der größten Ironie. Der Test las das Manifest als Volltext und suchte
`reqwest`. Der einzige Treffer:

```toml
# `harw-knowledge`-Kette (inkl. `harw-model-catalog` → `reqwest`/`tokio`)
```

— **genau der Kommentar, der dokumentiert, dass diese Kette entfernt wurde.**
Der Sicherheitstest schlug an, weil jemand erklärt hatte, was er beseitigt
hat.

Die zwei Vorgänger desselben Musters: K51 (ein `grep` nach `Surface::` traf
`InvocationSurface::`) und K61 (eine Suche nach `0.2.0` traf
`sd-listen-fds = "0.2.0"`). **Es gilt: eine Volltextsuche über strukturierten
Text prüft nicht, was sie behauptet.** Der Test liest jetzt die
Abhängigkeits*schlüssel* aus `[dependencies]` und `[build-dependencies]`,
schneidet Kommentare ab — und schließt `[dev-dependencies]` **ausdrücklich**
aus, mit Begründung: was dort steht, landet nie im Binary.

**Runde 4 ist wörtlich K61 an einem anderen Zähler.** Die damalige Korrektur
enthielt bereits die Warnung, die jetzt zählt: *„Beide bilden eine Differenz —
und selbst das reicht nicht."* Die sechs betroffenen Tests **maßen bereits
Differenzen** und fielen trotzdem um, weil zwischen Messung und Zusicherung
ein anderer Test denselben Zähler erhöhte.

**Daraus ein Suchlauf über den ganzen Baum:** sieben echte prozessweite
Nullzähler. Fünf abgesichert, **zwei nicht** —
`SECURITY_METRIC_LEAKED` (`harw-observe`) und
`OTLP_PROTECTED_NAMESPACE_BLOCKED` (`harw-observe-otlp`). Beide maßen
Differenzen, beide waren grün, **beide aus Glück.** Jetzt gesperrt.

**Warum gerade der erste teuer gewesen wäre:** `SECURITY_METRIC_LEAKED`
bewacht, dass ein `security.*`-Schlüssel **nie** einen exportierenden Sink
erreicht. Wird sein Test unter Last falsch grün, schweigt er über einen echten
Leak; wird er falsch rot, schaltet ihn irgendwann jemand als „flaky" ab.
**Beide Ausgänge sind schlecht, und nur einer fällt auf.**

*Methodisch:* jede der vier Ursachen wurde von einem Knoten gefunden, der
etwas anderes tat. Das ist der Ertrag des Anschließens — **eine Lücke wird
sichtbar, wenn zum ersten Mal jemand durchgeht.**

### Bottom-up-Analyse mit Plan-Abgleich

Das Abnahmekriterium des Ziels, mechanisch erhoben.

#### Teil 1: ist jeder Knoten gelandet?

Aus den Wellentabellen von `docs/aw-plan.md` gelesen, Schreibbereich je Knoten
gegen das Dateisystem geprüft (mit Brace-Expansion, alle Dateitypen):

| | |
|---|---|
| Knoten in den Tabellen | **88** |
| davon mit gefülltem Schreibbereich | **88** |
| Knoten mit Prosa-Schreibbereich (nicht auswertbar) | 2 (`AW1-03`, `AW1-01b`) -- beide von Hand geprüft, gelandet |
| Crates im Workspace | 95 |
| davon mit Inhalt | **95** |

**Kein leeres Gerüst mehr.** Der letzte war `harw-dod-cgroup` (AW2-13), den ich
als gelandet geführt hatte und der neun Zeilen lang war -- gefunden von der
`harw-dod`-Fassade, weil sie der erste Blick von außen auf den ganzen Teilbaum
war.

#### Teil 2: existiert ein Konsument?

Für jedes Crate: gibt es außerhalb seiner selbst, außerhalb von
`#[cfg(test)]` und **außerhalb von Doku-Kommentaren** eine Nennung in
fremdem `src/`-Code?

**Sieben Bibliotheken ohne Produktionskonsumenten**, davon zwei
Fehlklassifikationen und fünf echte Befunde:

| Crate | Urteil |
|---|---|
| `harw-dod-fixtures` | **Fehlalarm der Methode.** Reine Dev-Dependency, von sieben Sensorcrates in `[dev-dependencies]` benutzt -- genau dort, wo mein Skript nicht hinsieht. |
| `harw-channel-browser` | Bestand, hinter dem `browser`-Feature. Kein Befund dieses Programms. |
| `harw-dod-netpolicy` | **Kein Konsument, aber dokumentiert und bewusst.** `harw-dod-warden` trägt die Nahtstelle (`NetworkIsolator`-Trait); die echte `rustables`-Anbindung wurde aus Budgetgründen (K53) abgelehnt und der Grund im Code festgehalten. |
| `harw-dod` | Die Fassade. Gebaut, damit ein Konsument nicht 27 Pfadabhängigkeiten braucht -- und noch ohne. |
| `harw-observe-prom`, `harw-observe-otlp` | Zwei Sinks ohne Verdrahtung. Nur `harw-observe-file` wird tatsächlich benutzt (`harw-sentinel/src/main.rs`). |
| `harw-web` | Die HTTP-Fläche. Kein Prozess startet sie. |

**Was während der Analyse geschlossen wurde:**

- **`harw-dod-cgroup` und vier Geschwister**: der Sentinel registrierte vier
  von zehn Sensoren. Seine Moduldoku behauptete noch, fünf Crates
  „existieren noch nicht" -- alle fünf existierten. Jetzt fährt er **zehn**,
  samt `scanreport`, das sich nur anders konstruiert.
- **`harw-tool-lens`**: gebaut, um Lens seinen ersten Konsumenten zu geben,
  und selbst ohne. Jetzt in `RegistryProfile::Planning` eingetragen -- und
  ausdrücklich **nur** dort, weil `analyst`, `explorer` und
  `researcher-deps` sich ein Profil teilen und eine Aufnahme allen dreien das
  Werkzeug gegeben hätte.
- **`lens.ask` fehlte in `AUTO_APPROVED_TOOLS`.** Ein bestehender Test hat das
  gefangen: *jedes Werkzeug, das eine Rolle benutzen kann, muss von der
  Genehmigungsliste gedeckt sein* -- sonst hängt ein Fan-out an einer
  Rückfrage, die ein Kind mit `allow_pause = false` nie beantworten kann.

#### Teil 2 (zweite Erhebung, nach der Verdrahtungswelle)

Dieselbe Methode, erneut über alle **95** Crates gefahren, nachdem die
Kompositionswurzel, der OTLP-Transport, der eBPF-Lader, fanotify und die
Genehmigungsfläche gelandet waren.

**Elf Crates ohne Produktionskonsumenten — davon zehn erwartbar:**

| Crate | Urteil |
|---|---|
| `harw`, `harw-cli`, `harw-sentinel`, `harw-warden`, `harw-probe-fs`, `harw-probe-bpf`, `xtask` | **Binaries.** Ein Binary hat definitionsgemäß keinen Konsumenten im Baum. |
| `harw-dod-fixtures` | **Fehlalarm der Methode**, unverändert: reine Dev-Dependency von sieben Sensorcrates. |
| `harw-channel-browser` | Bestand, hinter dem `browser`-Feature. |
| `harw-dod-netpolicy` | **Dokumentierte Ablehnung**, jetzt mit drei geprüften Wegen (K63). |
| **`harw-dod`** | **Die einzige verbleibende Lücke** — und sie ist begründet abgelehnt, siehe unten. |

**Was diese Erhebung gegenüber der ersten geschlossen hat:**

- **`harw-observe-prom` und `harw-observe-otlp`** — beide waren „Sinks ohne
  jede Verdrahtung". Beide hängen jetzt hinter dem `RoutingSink` der
  Kompositionswurzel, **beide standardmäßig aus**, zuschaltbar über
  `--metrics-prometheus-port` und `--metrics-otlp-endpoint`.
- **`harw-web`** — war „die HTTP-Fläche, kein Prozess startet sie". Jetzt
  startet `harw web [--socket PATH]` sie auf einem Unix-Socket, und die
  Routentabelle entsteht aus **derselben** `build_operation_registry`, die
  `harw analyze` und der Chat benutzen. Kein zweiter Autoritätspfad.
- **`harw-tool-lens`** — hatte in der ersten Erhebung noch keinen Konsumenten
  und steht seither in `RegistryProfile::Planning`.

**Die eine offene Lücke, und warum sie offen bleibt.** `harw-dod` ist die
Fassade über 27 Sensor-, Regel- und Eskalationscrates. Der Knoten, der sie
hätte einhängen können, hat es **abgelehnt und begründet**: sie ist für einen
Beobachtungsdaemon gedacht, und das ist `harw-sentinel`s Aufgabe — nicht die
eines Control-Plane-Servers, der bereits autorisierte Operationen weiterreicht.
Sie hineinzuziehen wäre eine Abhängigkeit, die nichts tut.

**Das ist die Unterscheidung, die der Verifikationsplan für Schritt 7
verlangt:** ein ungenutztes Element ist nicht automatisch ein Versäumnis. Die
richtige Frage ist nicht „warum wird es nicht benutzt", sondern „löst es das
Problem, das der Code tatsächlich hat".

**Und die Telemetrie-Trennung ist strukturell, nicht bloß getestet:** die
Kompositionsstelle ruft `RoutingSink::approve_export` **nirgends** auf — es
gibt in diesem Modul keinen Codepfad, der eine `ExportApproval` herstellen
könnte. Prometheus und OTLP hängen ausschließlich an `route("app.", …)`.
`security.*` und `warden.*` können sie deshalb nicht erreichen, und der Beweis
ist die Abwesenheit eines Konstruktors, nicht ein grüner Test.

#### Teil 2 (dritte Erhebung, Endstand der Verdrahtungswelle)

Nach der zweiten Verdrahtungsrunde erneut über alle **95** Crates erhoben.
**Elf ohne Produktionskonsumenten, unverändert gegenüber der zweiten
Erhebung** — und die Zusammensetzung ist die entscheidende Aussage:

| Gruppe | Zahl | Urteil |
|---|---|---|
| Binaries (`harw`, `harw-cli`, `harw-sentinel`, `harw-warden`, `harw-probe-fs`, `harw-probe-bpf`, `xtask`) | 7 | **Erwartbar.** Ein Binary hat definitionsgemäß keinen Konsumenten im Baum. |
| `harw-dod-fixtures` | 1 | **Fehlalarm der Methode**: reine Dev-Dependency von sieben Sensorcrates. |
| `harw-channel-browser` | 1 | Bestand, hinter dem `browser`-Feature. |
| `harw-dod-netpolicy` | 1 | **Dokumentierte Ablehnung** mit drei geprüften Wegen (K63). |
| `harw-dod` | 1 | **Begründet abgelehnt**: eine 27-Crate-Sensorfassade gehört in einen Beobachtungsdaemon, nicht in einen Control-Plane-Server. |

**Keine Bibliothek dieses Programms ist mehr unverdrahtet, ohne dass der Grund
im Code steht.**

#### Schlussbilanz der Gates

| Stufe | Ergebnis |
|---|---|
| `cargo check --workspace` | 0 Fehler, 0 Warnungen |
| `cargo check --workspace --all-targets` | 0 Fehler, 0 Warnungen |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | Exit 0 |
| `cargo test --workspace` | **6616 bestanden, 0 gescheitert** |
| `trybuild`-Korpus | 35 `compile_fail`-Fälle, alle grün |

**Was bewusst offen bleibt, mit Ort und Grund:**

| Offen | Ort | Warum |
|---|---|---|
| `Assembly::gather` hat keinen Produktionsaufrufer | `harw-core/src/model.rs`, `ModelRequest::with_context_budget` | `gather_context` wandelt sofort auf `ContextFragment` zurück. Ein halb umgestellter Montagepfad wäre schlechter als ein unveränderter (K75). |
| `TrustClass` ist in der Praxis immer `Data` | `harw-operations/src/operation.rs:714` (`OpOutput`) plus 31 `EvidenceRef`-Stellen ohne Digest | Ein Klassenfeld auf `OpOutput` verschöbe die Frage, solange keine Operation etwas anderes herstellen kann. Die Kette ist **eine** Aufgabe (K77). |
| `validate_patch` hat keine Eingabe | `harw-cli/src/job_worker.rs`, zwischen Turn-Ende und `report_plan_node_outcome` | Nirgends entsteht ein `UnifiedDiff` (K66). |
| `EmbeddingRole::Confidential` hat keinen produktionsreifen Einbetter | `harw-lens-embed` | Ein lokales Modell kostet 171 Crates und einen C-Übersetzer (K71); der HTTP-Weg kann ihn strukturell nie bedienen. |
| `harw-dod-netpolicy` hat keinen Durchsetzer | `harw-warden/src/isolation.rs` | Drei geprüfte Wege, alle unvereinbar mit den geltenden Auflagen (K63). |
| Die eBPF-Objekterzeugung | eigener Bauschritt | Braucht `aya-ebpf`, `nightly` und das Ziel `bpfel-unknown-none` (K68). |

**Jeder dieser Punkte ist verortet, begründet und im Code dokumentiert — keiner
ist eine stille Lücke.** Das ist die Unterscheidung, die der Verifikationsplan
für Schritt 7 verlangt: *ein ungenutztes Element ist nicht automatisch ein
Versäumnis; die richtige Frage ist nicht „warum wird es nicht benutzt", sondern
„löst es das Problem, das der Code tatsächlich hat".*

#### Teil 3: woher stammt der Vergleichswert?

Die Frage, die dieses Programm sich selbst gestellt hat (K43). Vier Fundstellen
insgesamt, alle behoben:

| Fundstelle | Art |
|---|---|
| `harw_lens_query::query` | Abfrage-Manifest aus dem Index gebaut, den es prüfen sollte |
| `register_all_adds_nineteen_operations` | Array-Länge gegen sich selbst |
| `assert!(TELEMETRY_MAX_BYTES > 0)` | Zusicherung über eine positive Konstante |
| Versionskohärenz-Test | Volltextsuche nach `0.2.0`, traf die Fremdversion von `sd-listen-fds` |

### Der Contract-Master liegt unter `docs/aw-contract-master.md`

881 Zeilen, Abschnitte A–H, mit wörtlich zu übernehmenden Rust-Blöcken. Er
gilt, **solange die AW0-Crates nicht auf der Platte liegen**; danach gilt der
Code. Er entstand, weil die Umsetzung auf ausdrückliche Anweisung vollständig
parallel läuft und die Verifikation erst am Ende — ohne fixierte Signaturen
erfänden dreißig gleichzeitig arbeitende Agenten dreißig Fassungen desselben
Traits.

---

## Anschluss an das abgeschlossene Programm

Die vorige Ausbaustufe (Tags `baseline` … `w8-gates-green`) endete mit fünf
bewusst offenen Punkten. Das neue Programm greift **einen** davon auf:

| Offen aus W8 | Im neuen Programm |
|---|---|
| `ContextProgram.must_include`/`exclude` ohne Produktionskonsument | **AW1-03** — dort wird die Kontext-IR erstmals durchgesetzt. Verifiziert: alle heutigen Leser liegen in `#[cfg(test)]`. |
| `CellPlan::fanout_requests` ohne Konsument | nicht erwähnt |
| `LifecycleMachine.max_attempts` ohne Konsument | nicht erwähnt |
| `operations!` ungenutzt (bewusst) | nicht erwähnt |
| neun Fixture-Kopien in `harw-plan` | nicht erwähnt |

Ein Fundament, das das Programm voraussetzt, entstand erst in W7: der
`GoalContextProvider` erreichte zum Zeitpunkt des Schreibens **keinen
einzigen Turn**. Das Programm baut darauf, dass er die
Sicherheits-Invarianten trägt und „sie Compaction und Modellwechsel
überleben". Seit W7 trifft das zu — und die Goal-Invarianten dieses
Programms (S1, S4, S5, S6, K1, K2, K4, C1, C4, D1, L3, L4, U1, U2) werden
dort registriert.
