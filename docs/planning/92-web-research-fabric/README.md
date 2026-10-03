# PL-92 — Web Research Fabric: breite, nicht blockierende Recherche für die UIA

> **Status:** DRAFT  
> **Pinned baseline:** `mm9942/Harwness` `consolidate/main@677367d38bb6b940a809230cd62eca75210a415d` (Stand von PR #98). Die Aussagen in Abschnitt 2 wurden zusätzlich gegen `main@9e639f5` (Release 0.9.1) geprüft; zwischen beiden Ständen ändert sich am Web-/Recherche-Pfad nur ein Makro-Refactor in `delegate_wave.rs` (`KebabEnum`), keine Semantik.  
> **Scope:** Websuche und Web-Abruf so orchestrieren, dass die UIA breit (viele Quellen gleichzeitig), nicht blockierend und kontrolliert recherchieren kann  
> **Nicht-Ziel:** Training, Datensatzaufbau, Spiegeln ganzer Sites, Umgehen von Zugriffsbeschränkungen  
> **Regel:** Der Code am Baseline-Commit ist der Ist-Stand. Dieses Dokument beschreibt nur Deltas und gilt nie als umgesetzt, bevor Code, Tests und Doku nachgezogen sind (`../README.md` §2).

---

## 1. Ziel und Leitprinzip

Die UIA soll eine Rechercheanfrage („finde alles Belastbare zu X, Stand heute“) abgeben können und

1. **sofort** ein Handle zurückbekommen (kein Warten im Turn),
2. in der Zwischenzeit **viele Quellen parallel** abgearbeitet sehen,
3. **Teilergebnisse und Fortschritt** erhalten, steuern und abbrechen können,
4. am Ende ein **belegtes, dedupliziertes Evidenzpaket** bekommen (URL, Abrufdatum, Einstufung, Gegenquellen),
5. ohne dass irgendetwas davon in Trainingsdaten, Datensätze oder Exporte fließt: Das Material dient nur der **Information und Recherche**.

**Zu „DDoS-artig“.** Ich verstehe das als *Breite*: sehr viele Quellen gleichzeitig, hoher Gesamtdurchsatz. Das erreicht der Plan über **viele verschiedene Hosts parallel** und nicht über viel Last auf einem einzelnen Host. Pro Host bleibt die Last klein und höflich (Abschnitt 3.2). Das ist kein Zugeständnis, sondern die schnellere Variante: Wer einen Host hämmert, landet in 429-Spiralen, Captchas und IP-Sperren und recherchiert danach langsamer. Es deckt sich mit der Linie des Repos (DEC-003: Limits respektieren, sonst wirkt es „de facto wie eine DDoS“). Massenbedarf an *einem* Host (ganze Behördenportale, Wikipedia usw.) läuft über Sitemaps, APIs und Dumps und nicht über Crawling.

**Leitprinzip (zwei Ebenen trennen).** Heute geht jeder Abruf durch einen LLM-Agenten. Das ist der Engpass, nicht das Netz. Der Plan trennt:

| Ebene | Aufgabe | LLM? | Skaliert über |
|---|---|---|---|
| **Fetch-Fabric** | Warteschlange, Höflichkeit, Retry, Cache, Dedupe, Bereinigung | nein | viele Hosts, tausende URLs |
| **Reasoning-Ebene** | Fragen zerlegen, Treffer bewerten, extrahieren, gegenprüfen, verdichten | ja | Provider-Limits (DEC-003) |

---

## 2. CURRENT — Ist-Stand (gegen den Code verifiziert)

### 2.1 Matrix: Anforderung gegen Ist

Legende: ✔ vorhanden · ◐ teilweise · ✘ fehlt.

| Anforderung | Stand | Beleg / Anmerkung |
|---|---|---|
| Websuche | ◐ | `web.search`: 4 Backends (DuckDuckGo-HTML als Standard ohne Schlüssel, Brave, Tavily, SearXNG). **Eine** Anfrage je Aufruf, Query ≤ 400 Zeichen, 8 Treffer (hart 20), kein Cache. `harw-tool-web/src/search.rs:83-89`, Config `[web.search]` in `harw-config/src/web_toml.rs` |
| Abruf mit Policy | ✔ | `web.fetch`: nur GET, ohne Cookies/Header, jeder Hop durch `EgressPolicy`, SSRF/Rebinding-Schutz, Redirects manuell (≤ 5), 1 MiB Lesegrenze (hart 8 MiB), 64 KiB Ausgabe, 20 s Request-/45 s Gesamt-Deadline, Cache-TTL 1 h unter `<HARW_HOME>/cache/web/`. `harw-tool-web/src/fetch.rs:101-122`, `harw-egress/src/client.rs:110` |
| Weitere Netz-Tools | ✔ | `web.docs_rs`, `web.crates_io` (feste Hosts) |
| Mehrere Tool-Calls pro Modellturn parallel | ◐ | Alle `web.*` sind `parallel_safe`; der `JoinSet`-Pfad greift aber **nur**, wenn jeder Call `Allow` liefert. Bei `AskUser`/`Deny` fällt der Turn auf den sequenziellen Pfad. `harw-core/src/turn_loop.rs:6050` ff. |
| Offenes Web | ◐ | `[network].research_web = "open"` erlaubt jeden **öffentlichen** DNS-Host, nur lesend, für `researcher-web`, `researcher`, `dependency-researcher` (+ abgeleitete). Unter `ask`/`auto` fragt die **erste Anfrage je Domain** die Nutzerin; `full` fragt nicht. `harw-registry-defaults/src/research_web.rs:167`, `harw-tool-web/src/open_web.rs` |
| Agenten-Fan-out | ◐ | `delegate_wave`: 1..16 Ziele (`MAX_WAVE_TARGETS`), Standard `max_parallel = 4`, Join `all`/`any`/`collect`; ein **blockierender** Tool-Call im Orchestrator. `harw-core-bridge/src/delegate_wave.rs:121-124` |
| UIA nicht blockierend | ◐ | Orchestratoren laufen in der TUI im Hintergrund (`transfer_to_<rolle>`; Ergebnis kommt als Benachrichtigung; `agent.status/result/message/cancel`, `parent.message`, Fortsetzung ≤ 3). `docs/guides/background-agents.md`. **Aber** `/research`, `/research-web` starten genau **ein** Kind synchron (`harw-ops/src/research.rs`). |
| UIA-Worker parallel | ✘ (Absicht) | Die ganze `uia-worker`-Familie ist hart auf **1** parallele Instanz gedeckelt (`harw-runtime/src/children.rs:200`, Test `…caps_the_entire_uia_worker_family_at_one`). Budgets: `uia-explorer` 60 k Tokens / 40 Calls / 180 s, `uia-worker` 20 k / 16 / 180 s, `effort_cap = low`. |
| Höflichkeit pro Host (Rate, Concurrency) | ✘ | Kein Per-Host-Limit, keine globale Obergrenze gleichzeitiger HTTP-Anfragen, keine Pool-Konfiguration (`build_client` setzt nur Resolver, `no_proxy`, `redirect none`, `connect_timeout 10 s`). |
| robots.txt | ✘ | Nirgends im Code (`grep -ri robots harw-tool-web harw-egress` leer). Nur Text im Prompt von `intel-web-researcher` („Robots und Nutzungsbedingungen achte ich“), also **nicht erzwungen**. |
| 429 / `Retry-After` / Backoff fürs Web | ✘ | Gibt es nur für LLM-Provider (`harw-provider-http/src/rate_limiter.rs`, DEC-003), nicht für `web.*`. |
| Dedupe / URL-Kanonisierung / bedingte GETs | ✘ | Kein ETag/If-Modified-Since, keine URL-Normalisierung, kein Inhalts-Dedupe. |
| Batch-Abruf / Frontier / Run-Zustand | ✘ | Ein URL pro `web.fetch`; kein Run-Objekt, keine Queue. |
| Durables Orchestrierungssubstrat | ◐ | Jobs (`harw-job-core`, `JobKind::Custom`), `work_driver` als Beweis eines durablen Supervisors, `NotifyThrottle` (`harw-tool-job/src/throttle.rs`). Durable Chains (PL-69) sind **DRAFT**, kein Crate im Baum. |
| Evidenzvertrag | ✔ | `harw-research`: `ResearchQuestion`, `ResearchFinding`, `FindingBundle`, `SourceReference` (`locator`, `retrieved_at`, `digest`, `excerpt`, `reliability`, `credibility`, `derived_from`), `ReturnEnvelope`. |
| Evidenz-Persistenz | ◐ | `persist_finding` (`harw-ops/src/explore.rs:387`) schreibt **nur**, wenn Task, Finding-Store **und** Plan vorhanden sind; sonst `Ok(None)`. Ad-hoc-Recherche der UIA bleibt also ohne Plan flüchtig. |
| Analyse-Familie (LLM-Ebene) | ◐ | `intel-analysis-orchestrator` (4 Wellen: Sammeln → Prüfen → Verdichten → Belegcheck) mit `evidence-collector`, `pattern-analyst`, `systems-modeller`, `evidence-critic`, `method-auditor`, `synthesis-writer` (`harw-home/assets/agents/`). **Aber:** `evidence-collector` liest nur Dateikorpora (`may_research_web = false`), und der Web-Rechercheur `intel-web-researcher` (Evidenzpaket mit A–F/1–6-Einstufung) ist nicht Delegationsziel dieses Orchestrators, sondern hängt am Matrix-Game-Ablauf. Für Web-Sammlung stehen im `research-orchestrator` `researcher-web`, `researcher`, `dependency-researcher` bereit. |
| Web-Quellen im Wissensindex | ✘ | Lens-Indizes: `docs-design`, `palace`, `diary`, `code-rust` (`harw-lens-source/src/lib.rs`). Keiner für Web-Material. |
| Lokale Embeddings | ✔ | `harw-lens-embed`: `Locality::Local`, Rolle `Confidential` filtert fail-closed auf lokale Modelle. |
| Prompt-Injection-Grenze | ✔ | Zwei-Block-Konvention (Instruktions- vs. Datenblock), `harw-instructions/src/trust_boundary.rs`. |
| Schutz vor Trainings-/Dataset-Nutzung | ✘ (nichts nötig, nichts garantiert) | Im Repo existiert kein Trainings- oder Dataset-Exportpfad. Es gibt aber auch keine **erzwungene** Zusicherung („research only“) am Material. |

### 2.2 Wie die UIA heute recherchiert

```text
Pfad A — Schnellfrage    UIA ─► uia-worker (1×, 16 Calls, 180 s, web.search/fetch/docs_rs/crates_io)
Pfad B — gebundene Frage UIA ─► /research-web ─► ein uia-explorer-Kind (synchron, 40 Calls, 180 s)
Pfad C — breit           UIA ─► root-orchestrator (Hintergrund) ─► research-orchestrator
                                   └─ delegate_wave: ≤ 16 Rechercheure (researcher-web, researcher, …),
                                      Standard 4 parallel, blockierend im Orchestrator
```

Nur Pfad C ist breit und nicht blockierend für die UIA. Er hat die drei Schwächen aus 2.1: jeder Abruf geht durch ein LLM, jede neue Domain fragt die Nutzerin (der Turn fällt dann auf den sequenziellen Pfad), und es gibt keine Höflichkeits-/robots-/Backoff-Schicht.

### 2.3 Dokumentationsdrift (gefunden, Code gewinnt)

- `harw-tool-web/src/lib.rs` beschreibt „drei Tools“; es sind vier (`web.search` fehlt in der Tabelle), dazu `open_web`.
- `harw-registry-defaults/src/research_web.rs` (Moduldoku) sagt, `WebToolProvider` habe noch keinen Konstruktor mit Policy („Folgearbeit W6“); `harw_tool_web::configure` und `install_web_tools` existieren.
- Es gibt **keine** Anwenderdoku für `[network].research_web`, `[network].researcher_web_hosts`, `[web.search]`, `[research]`. `docs/setup/web.md` beschreibt die Kontrollfläche `harw web` und nicht die Web-Recherche (Namensverwechslung).

### 2.4 Was ich unter „Exports“ verstanden habe

Im Repo liegen keine Export-Dateien (kein Chat-/Tool-Export). Ich habe daher die **öffentlichen Exports** der betroffenen Crates (`harw-tool-web`, `harw-egress`, `harw-research`, `harw-core-bridge`, `harw-tool-job`, `harw-registry-defaults`, `harw-lens-*`) und die Docs gelesen. Die TUI-`/export`-Funktion (`harw-tui/src/export.rs`) exportiert nur Chat-Verläufe und ist hier nicht relevant. Gemeint waren Exports aus einer anderen Quelle (z. B. frühere Chat-Exporte)? Dann bitte den Pfad nennen.

---

## 3. PLANNED — Zielarchitektur

### 3.1 Überblick

```text
UIA ──research_run start {charter}──► ResearchRun (JobKind::Custom("web_research"), durabel)
 ▲        │ sofort: run_id                    │
 │        │                                   ▼
 │   Benachrichtigung (gedrosselt)      ┌─────────────────────────────────────────────┐
 │   Teilergebnisse (results since=)    │ Reasoning-Ebene (LLM, Provider-Limits)      │
 │   steer / cancel                     │  Planner (1×) ─► Fragen-DAG (ResearchQuestion)│
 │                                      │  Query-Expansion ─► web.search_many         │
 │                                      │  Triage (billig/lokal) ─► Kandidaten-URLs   │
 │                                      │  Extraktoren (N, ohne Tools) ─► Findings    │
 │                                      │  Verifier/Critic ─► Gegenquellen            │
 │                                      │  Synthesizer (1×) ─► ReturnEnvelope         │
 │                                      └───────────▲───────────────┬─────────────────┘
 │                                       URLs/Queries │             │ bereinigte Chunks (Backpressure)
 │                                      ┌─────────────┴───────────────▼─────────────────┐
 │                                      │ Fetch-Fabric (kein LLM)                       │
 │                                      │  Frontier · faire Domain-Queue · Limits       │
 │                                      │  robots · Retry/Backoff · Cache · Dedupe      │
 │                                      │  HTML→Text · Chunking · lokales Embedding     │
 │                                      └─────────────┬─────────────────────────────────┘
 │                                                    ▼
 └──────────────────────────────────────── Evidence-Store (Roh-Cache + Manifest + Findings + lens-Index)
```

### 3.2 Fetch-Fabric (neues Crate `harw-web-fabric`)

Ein Scheduler, den **alle** Netz-Abrufe nutzen. Auch ein einzelnes `web.fetch` läuft dann höflich. Alle Zahlen sind **Vorschläge** und per Config änderbar.

| Baustein | Regel |
|---|---|
| Globale Obergrenze | `max_in_flight` = 64 gleichzeitige Anfragen (zählt Anfragen, nicht TCP-Verbindungen; HTTP/2 multiplext). |
| Faire Queue | Round-Robin **über Domains** (registrierbare Domain), innerhalb einer Domain nach Nutzwert. Das erzwingt Breite statt Tiefe und verhindert, dass ein langsamer Host die Queue blockiert. |
| Pro Host | `max_in_flight_per_host` = 2, Mindestabstand 1 s (oder `Crawl-delay`, falls größer); unbekannte Hosts starten konservativ bei 1 parallel. |
| Adaptiv (AIMD) | Bei 429/503/Timeout-Häufung Rate ×0,5 (Untergrenze 1 Anfrage/10 s); nach 20 Erfolgen in Folge +1 bis zur Host-Obergrenze. |
| Circuit Breaker | 5 Fehlschläge in Folge ⇒ Host 60 s offen, dann ein Probe-Request (half-open). |
| `Retry-After` | Wird befolgt (bis 120 s; länger ⇒ URL zurückstellen und Host bestrafen). |
| Retry | Nur idempotente GETs, 3 Versuche, exponentiell mit Jitter, harte Gesamt-Deadline je URL. |
| robots.txt | Je Origin laden (TTL 24 h, höflich begrenzt), RFC 9309: `Disallow`/`Allow`/`Crawl-delay` für den UA-Token `harwness-research`; 4xx ⇒ erlaubt, 5xx ⇒ vorübergehend gesperrt. Verstoß ⇒ URL wird **nicht** abgerufen, Grund im Manifest. |
| Opt-out-Signale | `X-Robots-Tag` (`noai`, `noindex` …), `<meta name="robots">`, `tdm-reservation`-Header und `/.well-known/tdmrep.json` auswerten und im Manifest als Flag `tdm_reserved` festhalten (Behandlung: Entscheidung E3). |
| Bedingte GETs | ETag/If-Modified-Since, Stale-While-Revalidate; TTL nach Content-Type. |
| Dedupe | URL-Kanonisierung (Tracking-Parameter, Fragment, Query-Sortierung), Inhalts-Hash (BLAKE3, wie der bestehende Cache), später Near-Dup (SimHash). |
| Run-Budgets | je Run: `max_urls` (500), `max_bytes` (256 MiB), `max_domains` (200), `max_wall` (30 min). Hart. |
| Backpressure | Der Prefetch-Puffer ist **byte-begrenzt** (64 MiB). Ist er voll, holt die Fabric nichts Neues, bis die Reasoning-Ebene verbraucht hat. |
| Abbruch | Kooperativ über das vorhandene `CancelToken`; Teilergebnisse bleiben erhalten. |
| Zeit | Uhr injizierbar (Muster: `NotifyThrottle` nimmt `Instant` entgegen), damit Limits deterministisch testbar sind. |

**Durchsatz-Überschlag.** Bei 64 gleichzeitigen Anfragen über ≥ 64 verschiedene Hosts und ~1 s Latenz sind mehrere Dutzend Abrufe pro Sekunde möglich. 500 URLs sind in unter ~2 Minuten geholt. Die LLM-Ebene (Provider-`max_concurrency`, 5–10 s je Extraktion) liefert eher ~1 Dokument/s. **Deshalb wird vor dem LLM gefiltert:** bereinigen → chunken (`harw-lens-chunk`) → lokal einbetten → nach Relevanz zur Frage ranken → nur die Top-k-Chunks an die Extraktoren. Das spart Kosten und Zeit und hält Rohtext lokal.

**Architektur-Gate.** `harw-web-fabric` kommt in `xtask/arch-policy.toml` als Schicht **I** (Abhängigkeiten: `harw-egress` I, `harw-research` I, `harw-fsutil`, `harw-lens-chunk`/`-embed`); `harw-tool-web` (Schicht A) darf davon abhängen.

### 3.3 Quellen-Tiers (API-first)

1. **Lizenzierte Such-APIs und Selbstbetrieb** (Brave, Tavily, eigene SearXNG-Instanz): Hier skaliert die Suche. Host-Limits kommen aus der Config (`[research.fabric.hosts."api.search.brave.com"]`), nicht aus dem Bauch.
2. **Offizielle Maschinenquellen** (APIs, Sitemaps, RSS/Atom, Dumps, `docs.rs`, `crates.io`, Register): bevorzugt. Das ist auch inhaltlich die bessere Evidenz (Primärquelle).
3. **Normale Webseiten**: Fabric-Regeln wie oben, Tiefe 0 (nur Suchtreffer und explizite Seeds). Link-Following (Tiefe ≤ 1) ist optional und per Charter zu aktivieren (E6).
4. **DuckDuckGo-HTML**: bleibt nur als schlüsselloser **Fallback** mit eigenem harten Limit (≈ 1 Anfrage/s global). Das Scrapen von Ergebnisseiten ist fragil und Nutzungsbedingungs-kritisch und wird nicht skaliert.

### 3.4 Werkzeugoberfläche

Agenten-Tools (alle `NetworkAccess`-geprüft **vor** den Argumenten, `parallel_safe`, laufen über die Fabric):

| Tool | Argumente | Ergebnis |
|---|---|---|
| `web.search_many` | `queries[≤16]`, `site?`, `max_results` | zusammengeführte, deduplizierte Trefferliste mit Herkunft je Query |
| `web.fetch_many` | `urls[≤50]`, `format`, `max_bytes_each` | kompaktes Ergebnis je URL (Status, Titel, bereinigter Auszug, Cache-Handle, robots-/TDM-Flags); Gesamtausgabe gedeckelt. **Ein** Modellaufruf ersetzt bis zu 50 LLM-Runden. |

Operation für UIA/Orchestratoren (Muster wie `agent.status`/`agent.result`):

| Operation | Zweck |
|---|---|
| `research_run start {charter}` | legt den Run an, gibt **sofort** `{run_id}` zurück |
| `research_run status {run_id}` | Phase, Zähler (URLs offen/fertig/abgelehnt, Domains, Bytes, Findings), Budgetrest, Host-Gesundheit |
| `research_run results {run_id, since?}` | Teilergebnisse seit einem Cursor (paginiert, wie `agent.result`) |
| `research_run steer {run_id, …}` | Frage hinzufügen, Domain sperren, Budget anheben (Anheben nur mit Nutzerfreigabe) |
| `research_run cancel {run_id}` | kooperativer Abbruch; Teilergebnisse bleiben |

**Charter** (Auftragsrahmen): Fragen, Zeitbezug/Freshness, erlaubte Quellklassen (`SourceClass`), Sprachen, Domain-Sperrliste, Budgets, Tiefe, Ablage-/Aufbewahrungsregel, Extraktions-Providerklasse (lokal oder API ohne Training).

### 3.5 Freigabe im offenen Web (die wichtigste Entscheidung)

Heute fragt die **erste Anfrage je Domain** die Nutzerin (`open_web.rs`; im Prompt von `intel-web-researcher` ausdrücklich „gewollt“). Bei Hunderten Domains blockiert das den Run und ist nicht praktikabel; außerdem zwingt es `turn_loop` auf den sequenziellen Pfad.

Vorschlag **Run-Freigabe statt Domain-Freigabe**: Die UIA legt der Nutzerin die Charter **einmal** vor (Fragen, Quellklassen, Budgets, Sperrliste, geschätzte Domainzahl). Bestätigt sie, gilt für **diesen Run**: öffentliches DNS lesend (`EgressTarget::PublicDns`), mit Ablauf und den Budgets als harte Grenze. Das nutzt den vorhandenen `OpenWebAccess::grant`, begrenzt auf Run-ID und Ablaufzeit. Unverändert bleiben: Private/Loopback/Link-Local nie, Sperrliste hat immer Vorrang, Redirects auf neue Domains werden nicht automatisch erlaubt (sie landen als Kandidaten in der Queue und gehen durch dieselbe Prüfung), alle abgerufenen Domains stehen im Audit. Der Domain-Prompt bleibt für Pfad A/B und für Läufe ohne Charter unverändert. Das ändert eine bewusste frühere Entscheidung und braucht deshalb deine Freigabe (E5).

### 3.6 Reasoning-Ebene

Wiederverwendet statt neu gebaut: `intel-web-researcher` (Evidenzpaket mit A–F/1–6-Einstufung) und `researcher-web` als Web-Worker, `evidence-critic`, `method-auditor`, `synthesis-writer` für Prüfen und Verdichten, der Vertrag `research-finding@1` und `harw-research::FindingBundle`. Die Wellen steuert der Run-Controller deterministisch (wie `work_driver` seine Worker) und startet die LLM-Rollen als Worker; ein eigener LLM-Orchestrator ist dafür nicht nötig. `intel-analysis-orchestrator` bleibt unverändert für Korpusanalyse.

Neu ist nur, was fehlt:

| Rolle | Aufgabe | Tools |
|---|---|---|
| `research-planner` (1×) | zerlegt in einen Fragen-DAG mit expliziter Frageneignerschaft, Quellgrenzen und Stopp-Bedingung (`coding-philosophy.md` §4) | keine Netz-/Workspace-Tools; liest Palace/Diary |
| `research-extractor` (N) | liest **einen** bereinigten Chunk-Satz, liefert ausschließlich schema-validiertes `ResearchFinding`-JSON | **keine** Tools (Injection-Fläche null) |
| Triage (billig/lokal) | rankt Treffer nach Titel/Snippet/Embedding, bevor gefetcht oder extrahiert wird | keine |

Ablauf (Wellen mit Synthese-Barriere):

1. **Plan** → Fragen-DAG.
2. **Sammeln:** je bereite Frage `web.search_many` → Kandidaten → Triage → Fabric → Chunks → Extraktoren. Fetch läuft per Prefetch voraus, Extraktion zieht mit Provider-Tempo nach.
3. **Prüfen:** tragende oder riskante Behauptungen **absichtlich doppelt** aus unabhängigen Domains (Regel 4 der Philosophie); `evidence-critic` sucht Gegenquellen und Widersprüche; Unbekanntes bleibt sichtbar.
4. **Verdichten:** `synthesis-writer` (1×) → `ReturnEnvelope` mit Zitaten; Belegcheck vor Abschluss.

**Provider-Limits.** Extraktoren teilen die vorhandene Provider-Instanz samt `ProviderRateLimiter` (DEC-003); es wird nie ein eigener ungedrosselter Client aufgemacht. `effective_parallel = min(Extraktoren, Provider-max_concurrency)`.

**Träger.** Die Zielform ist eine durable Chain (PL-69, DRAFT). Phase 3 startet nicht davon abhängig, sondern mit `JobKind::Custom("web_research")` nach dem Muster von `work_driver` (durabler Supervisor mit Sidecar-Zustand) und zieht später auf das Chain-Substrat um.

**Was bewusst nicht geändert wird.** `uia-worker`-Familie bleibt auf 1 Instanz gedeckelt (Absicht), `delegate_wave` bleibt bei 16 Zielen und blockierend im Orchestrator (andere Aufgabe). Breite Netz-Last läuft **nicht** über mehr LLM-Kinder, sondern über die Fabric.

### 3.7 Nicht blockierend aus Sicht der UIA

1. `research_run start` kehrt sofort zurück; der UIA-Turn endet mit 1–3 Sätzen (wie bei Hintergrund-Agenten).
2. Fortschritt kommt **gedrosselt** als Benachrichtigung: alle 60 s und nur bei Änderung, Fehler entprellt zu einer Meldung (gleiche Regeln wie `NotifyThrottle`).
3. Teilergebnisse sind abrufbar, bevor der Run fertig ist (`results since=`).
4. Lenken und Abbrechen jederzeit; `/new` bricht laufende Runs wie Hintergrund-Agenten ab; Freigabe-Prompts erscheinen auch bei untätiger TUI.
5. Das Endergebnis startet einen UIA-Turn, sobald die UIA idle ist.
6. Der Run überlebt Prozesswechsel (durabel, Checkpoint je Frage und je Dokument).

### 3.8 Evidence-Store und „kein KI-Training“

**Ablage.** `<HARW_HOME>/research/<run_id>/` (`0700`/`0600` über `harw-fsutil`, wie der Web-Cache): `manifest.jsonl` (URL, End-URL, Status, Abrufzeit, Inhalts-Hash, Bytes, robots-Entscheid, `tdm_reserved`, Lizenzhinweis falls erkennbar), bereinigter Text, Findings, `FindingBundle`. Dazu ein neuer Lens-Index `research-web` (`harw-lens-source`) mit TTL. Evidenz wird **auch ohne Plan** persistiert (behebt das `Ok(None)` aus 2.1).

**Garantien (erzwungen, nicht nur dokumentiert):**

| Garantie | Umsetzung |
|---|---|
| Material trägt `use_policy = research_only` | Pflichtfeld im Manifest und in jedem Finding-Export; nicht unterdrückbar |
| Kein Export in Datensätze | Architektur-Test: kein Schreibpfad von `research/` und Web-Cache in Dataset-/Trainingsformate; `/export` und der Agent-Compiler schließen das Verzeichnis aus |
| Rohtext bleibt lokal, wo möglich | Embeddings für Web-Dokumente nur mit `Locality::Local` (Fail-Closed wie bei `Confidential`); Extraktion nur mit Providerklasse `local` oder `api_no_train`; unbekannte Datenverwendung ⇒ Warnung und Freigabe nötig |
| Nur so viel wie nötig gespeichert | kurze Auszüge (`excerpt` gekappt) plus Link statt Volltext, wenn `tdm_reserved`; Aufbewahrung 30 Tage (konfigurierbar), `harw research purge` |
| Ehrlicher User-Agent | `harwness-research/<ver> (+Kontakt-URL)`, damit Betreiber ausschließen können |
| Keine Personendaten-Sammlung | Extraktions-Schema ohne Personenfelder; Regel aus `intel-web-researcher` („keine Personendaten über Privatpersonen“) wird im Schema zur Pflicht |

Rechtlicher Hinweis (keine Rechtsberatung, vor Produktivbetrieb prüfen): Für Text und Data Mining gelten in Deutschland § 44b UrhG (mit maschinenlesbarem Nutzungsvorbehalt bei online zugänglichen Werken) und § 60d UrhG (wissenschaftliche Forschung). Der Plan befolgt maschinenlesbare Vorbehalte standardmäßig (robots, `tdm-reservation`, `noai`) und speichert dann höchstens kurze Auszüge samt Quelle. Wie streng das sein soll, ist Entscheidung E3.

### 3.9 Beobachtbarkeit

Über `harw-observe` (OTLP/Prometheus-Sinks existieren): `fabric_in_flight`, `fabric_queue_depth`, je Host p50/p95-Latenz, 429-/5xx-Zähler, Breaker-Zustand, robots-Ablehnungen, Cache-Trefferquote, Bytes, Prefetch-Füllstand, Extraktions-Durchsatz und -Kosten je Run. TUI: Ansicht `/research-run` (Fortschritt, langsamste Hosts, Ablehnungen).

### 3.10 Konfiguration

Neuer Block `[research.fabric]` (Defaults wie 3.2; Provider-Namen und Hostlimits je Tabelle). **Vertrauensregel:** Ein nicht vertrauter Repo-Layer darf nur **verengen** (niedrigere Limits, mehr Sperren, `research_web` nur Richtung `allowlist`), nie erweitern; das ist dasselbe Muster wie in `harw-config/src/discovery.rs` (restricted layer). Schlüssel kommen nur aus Umgebungsvariablen (`api_key_env`), nie aus Dateien.

---

## 4. DELTA — Arbeitspakete

Reihenfolge nach Nutzen. P0–P2 bringen schon ohne Run-Controller Verbesserungen, auch für die Pfade A/B.

| # | Paket | Inhalt | Betroffen | Akzeptanz | Größe |
|---|---|---|---|---|---|
| **P0** | Messung und Doku-Drift | (a) Pilot heute: UIA → root-orchestrator → `research-orchestrator`, 8–16 `researcher-web` per `delegate_wave`, `research_web = "open"`, um reale Engpässe zu messen (Domain-Prompts, Latenz, Budgets). (b) Doku-Drift aus 2.3 beheben; neue Anwenderdoku `docs/setup/research-web.md` (`[network]`, `[web.search]`, `[research]`). | Docs, `harw-tool-web/src/lib.rs`, `research_web.rs` | Messbericht mit Zahlen; Doku stimmt mit Code überein | S |
| **P1** | Fetch-Fabric-Kern | Crate `harw-web-fabric`: Queue, Domain-Fairness, Limits, AIMD, Breaker, `Retry-After`, Retry, robots, bedingte GETs, Dedupe, Budgets, Backpressure, Metriken; `web.fetch` läuft darüber | neues Crate, `xtask/arch-policy.toml`, `harw-tool-web`, `harw-egress` (Client-Pool/Limits) | siehe Tests 5 (virtuelle Zeit, 1000 Tasks halten Host-Cap; robots-Fälle; 429-Test; Fairness ohne Aushungern) | L |
| **P2** | Batch-Tools und Suche | `web.fetch_many`, `web.search_many`; Such-Backends als erstklassig (Brave/Tavily/SearXNG), DDG nur Fallback mit 1-rps-Limit; Treffer-Dedupe; Rollen-TOMLs (`uia-explorer`, `researcher-web`, `researcher`, `intel-web-researcher`) um die Tools erweitern | `harw-tool-web`, `harw-registry-defaults/agents/*.toml`, **Golden-IR neu erzeugen** (`tests/golden_ir/*.json`), `rights_matrix`, `registry_invariants` | Batch von 50 URLs in **einem** Modellaufruf; Rechte-Matrix grün; `uia-worker` ohne neue Rechte jenseits der Eltern | M |
| **P3** | Run-Controller und UIA-Oberfläche | `research_run`-Operation, `JobKind::Custom("web_research")`, Persistenz/Checkpoints, gedrosselte Benachrichtigungen, TUI-Ansicht, **Charter-Freigabe** (E5), `uia.md`-Regelwerk („wann Pfad A/B/C/Run“) | `harw-ops`, `harw-job-*`, `harw-tui`, `harw-registry-defaults/knowledge/roles/uia.md`, `open_web.rs` | UIA-Turn endet < 1 s nach `start`; Teilergebnisse abrufbar; Abbruch räumt auf; Neustart setzt Run fort | L |
| **P4** | Reasoning-Ebene | `research-planner`, `research-extractor`, Triage; Verdrahtung des Run-Controllers mit `intel-web-researcher`/`researcher-web`, `evidence-critic`, `synthesis-writer` (Delegationsziele und Spawn-Matrix prüfen); Vertrag `research-bundle@1` über `FindingBundle`; Backpressure und `effective_parallel` | `harw-registry-defaults` (Rollen, Kontextprogramm `research-web`, Golden), `harw-research` | Extraktor-JSON schema-valide oder abgelehnt; Fetch-Vorlauf stoppt bei vollem Puffer; Provider-Limits nie überschritten | L |
| **P5** | Evidence-Store und No-Train-Schutz | Ablage, Manifest, Lens-Index `research-web`, `use_policy`, Retention/Purge, Locality-Local-Embeddings, Arch-/Gate-Test „kein Export in Datensätze“ | `harw-lens-source`, `harw-lens-embed`, `harw-tui` (`/export`), `harw-agent-compiler`, `xtask` | Test schlägt fehl, sobald ein Pfad `research/` in ein Dataset-Format schreibt; Purge löscht nachweislich | M |
| **P6** | Härtung und Lasttest | synthetisches „Internet“ (≥ 500 Hosts, injizierte 429/5xx/Langsam/robots), Soak, Chaos (Prozess-Kill mitten im Run), Kosten-/Budget-Grenzen, Sicherheitsreview | Tests, `xtask` | Ziele aus 5 erreicht; kein Host-Limit je verletzt; Fortsetzung nach Kill ohne Doppelabruf | M |

Abhängigkeiten: P1 → P2 → P3 → P4; P5 kann nach P1 parallel zu P3 laufen; P6 begleitet ab P1 und schließt ab.

---

## 5. Tests und Lasttest

| Bereich | Test |
|---|---|
| Limits | Property-Test: bei N = 1000 Tasks über M Hosts wird weder das globale noch ein Host-Limit je überschritten (virtuelle Uhr) |
| Fairness | kein Aushungern: jeder Host kommt innerhalb einer Rundenzahl dran, auch bei einem sehr langsamen Host |
| robots | RFC-9309-Fälle (Gruppen, `Allow` vs. `Disallow`, `Crawl-delay`, 4xx/5xx, riesige Datei); Ablehnung steht im Manifest |
| Backoff | 429 mit `Retry-After`: Wartezeit eingehalten; AIMD senkt und erholt |
| Egress | bestehende SSRF-/Rebinding-/Redirect-Tests bleiben grün; neu: Redirect auf neue Domain landet in der Queue, wird nicht automatisch abgerufen |
| Durabilität | Kill mitten im Run: Fortsetzung ohne Doppelabruf, Budgets stimmen |
| Rechte | `rights_matrix`, `registry_invariants`, Golden-IR; Extraktoren haben nachweislich **keine** Tools |
| No-Train | Gate-Test (P5) |
| Last | 500 Hosts, 5 % 429, 2 % 5xx, 5 % langsam (> 10 s): Fetch-Ebene ≥ 30 Abrufe/s im Mittel, LLM-Ebene ist der Engpass, nicht die Fabric |

Die Zielzahlen sind Hypothesen aus 3.2 und werden in P0 und P6 an echten Messwerten korrigiert.

---

## 6. Sicherheit und Missbrauchsschutz

- **Prompt-Injection:** Abgerufener Text ist Datenblock (Zwei-Block-Konvention); Extraktoren haben keine Tools und liefern nur schema-validiertes JSON; Anweisungen auf Webseiten werden gemeldet, nicht befolgt.
- **Exfiltration:** Die A5-Regel bleibt: wer ins Netz schreiben kann, liest nichts Lokales. Extraktoren und `researcher-web` haben keinen Workspace-Zugriff; die Fabric sendet nur GET ohne Cookies/Credentials.
- **SSRF/Rebinding:** unverändert über `EgressPolicy`/`build_client`; die Fabric darf **keinen** zweiten Client ohne diese Policy bauen.
- **Queue-Vergiftung / URL-Explosion:** harte Run-Budgets, Link-Following aus, Tiefe ≤ 1, Domain-Sperrliste.
- **Selbstschutz des Hosts:** globale Obergrenze, Byte-Puffer, Dateihandles; ein Run kann den Rechner nicht erschöpfen.
- **Fremdlast:** keine Mehr-Instanz-Tricks zum Umgehen von Host-Limits. Ein Prozess, eine Fabric, eine Policy.
- **Audit:** jeder Abruf (URL, Entscheid, Run-ID, Rolle) wird protokolliert.

---

## 7. Risiken

| Risiko | Gegenmaßnahme |
|---|---|
| Domain-Freigabe pro Run ist eine Lockerung einer bewussten Entscheidung | Charter muss sichtbar bestätigt werden, Ablauf und Budgets hart, Sperrliste vorrangig; Prompt-pro-Domain bleibt Standard außerhalb von Runs (E5) |
| DDG-Scraping oder andere HTML-Suchen werden gesperrt | API-first-Tier, DDG nur Fallback; mehrere Backends parallel |
| Vorbehalte (robots/TDM) reduzieren die Ausbeute | Meldung im Ergebnis („x Quellen wegen Vorbehalt übersprungen“), Primärquellen/APIs bevorzugen |
| LLM-Kosten explodieren bei breiter Sammlung | Vorfilter (lokales Ranking), Budgets, billiges/lokales Modell für Extraktion |
| Stale Docs führen in die Irre | P0 beseitigt die gefundene Drift; Test, dass Rollen-TOMLs und Doku-Tabelle übereinstimmen |
| Durable Chain (PL-69) verzögert sich | P3 startet mit `JobKind::Custom`, Umzug später |

---

## 8. Offene Entscheidungen (Standard = Vorschlag)

| # | Frage | Vorschlag |
|---|---|---|
| E1 | Größenordnung: wie viele URLs/Stunde, wie viele parallel? | `max_in_flight` 64, 2 je Host, 500 URLs je Run; nach P0-Messung anpassen |
| E2 | Welche Suchbackends hast du bzw. willst du? | Brave und/oder eigene SearXNG; DDG nur Fallback; Tavily optional |
| E3 | Umgang mit maschinenlesbaren Vorbehalten (robots/`tdm-reservation`/`noai`) | respektieren: nicht abrufen bzw. nur Auszug + Link speichern; alternativ „abrufen, aber nichts speichern“ |
| E4 | Wo läuft die Extraktion: lokales Modell oder API? | lokal oder `api_no_train`; Rohtext-Embeddings immer lokal |
| E5 | Run-Freigabe per Charter statt Domain-Prompt (3.5) | ja, nur für `research_run`; Prompt-pro-Domain bleibt sonst |
| E6 | Link-Following (Tiefe ≤ 1) oder nur Suchtreffer und Seeds? | zunächst nur Suchtreffer und Seeds |
| E7 | „Exports“: waren andere Dateien gemeint (2.4)? | Pfad nennen, ich gleiche den Plan ab |

---

## 9. REFERENCES

- Code: `harw-tool-web/src/{search,fetch,open_web,hop,cache}.rs`, `harw-egress/src/{policy,client}.rs`, `harw-registry-defaults/src/research_web.rs`, `harw-registry-defaults/agents/{uia-worker,uia-explorer,researcher-web,research-orchestrator}.toml`, `harw-home/assets/agents/{intel-web-researcher,intel-analysis-orchestrator,evidence-collector}/`, `harw-core-bridge/src/delegate_wave.rs`, `harw-runtime/src/children.rs:200`, `harw-ops/src/research.rs`, `harw-research/src/`, `harw-tool-job/src/throttle.rs`, `harw-core/src/turn_loop.rs`, `harw-config/src/{web_toml,network_toml,research_toml}.rs`, `harw-lens-source`, `harw-lens-embed`, `xtask/arch-policy.toml`.
- Docs: `docs/philosophy/coding-philosophy.md` §4 (Research Can Fan Out Aggressively), `docs/guides/background-agents.md`, `docs/planning/69-durable-agent-chains/README.md`, `docs/planning/70-decisions/DEC-003-provider-limits.md`, `DEC-005-small-scopes-waves.md`, `docs/design/agents-as-tools.md`, `docs/design/config-scopes.md`.

## 10. IMPLEMENTATION STATUS

Nicht begonnen. Nichts in diesem Dokument ist umgesetzt; alle Zahlen in 3.2 sind Vorschläge. Der Ist-Stand steht ausschließlich in Abschnitt 2 und ist gegen den Code am Baseline-Commit geprüft.
