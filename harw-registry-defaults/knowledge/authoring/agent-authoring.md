<!-- harwness.knowledge.agent-authoring@1 -->
# Bauplan: Agentendefinitionen (TOML, `harwness.agent/v1`)

## Rolle zuerst wählen
Eine Agentendefinition trägt genau eine `role` aus den bekannten
Organisationsrollen: `worker`, `uia-worker`, `user-interface`,
`root-orchestrator`, `child-orchestrator`, `agent-steward`. Die Rolle
bestimmt Sichtbarkeit, Elternteil, Rechte-Obergrenze, Effort/Modellstufe und
das angehängte Regelwerk. Spezialisierung (`specialization`, `description`)
ist nur **Inhalt** — sie erweitert nie die Rechte, die die Rolle vorgibt.
Ein Agent bekommt genau das Werkzeugprofil seiner Rolle, nie mehr.

## Pflichtfelder
```toml
schema = "harwness.agent/v1"
id = "harwness.agent.<name>@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "<name>"

name = "<Anzeigename>"
description = "<Kurzbeschreibung>"
```
`extends` bindet die Basisdefinition (meist `worker-base@1`), die
gemeinsame Vorgaben (Lifecycle, Rückgabevertrag-Grundform) trägt. `id`
trägt eine Versionszahl (`@1`); eine neue, inkompatible Fassung bekommt
`@2` statt die alte zu überschreiben.

## [tools]
```toml
[tools]
admitted = ["fs.read", "fs.list", ...]
forbidden = ["fs.write", "shell.exec", ...]
```
`admitted` muss **exakt** die Werkzeuge des zugeordneten Registry-Profils
treffen (`harw-registry-defaults/src/profile.rs::profile_for_role`) — nicht
mehr, nicht weniger (Deckungstest `tool_admission_coverage.rs`). `forbidden`
nennt sicherheitsrelevante Werkzeuge ausdrücklich, die die Rolle NICHT
zulässt (etwa `fs.write`/`shell.exec` bei einer read-only Rolle), damit eine
Ablehnung begründet ist statt nur „nicht in admitted“.

## [spawn]
```toml
[spawn]
max_depth = 0

[spawn.budget]
max_tokens = 60000
max_tool_calls = 40
max_wall_secs = 180
effort_cap = "low"   # optional
```
`max_depth = 0` heißt: unter dieser Rolle entsteht keine weitere Ebene (ein
in sich geschlossener Lauf ohne eigene Kinder) — die meisten Worker tragen
`1` (aus der Basis geerbt), nur der Analyst darf `2`.

## [return]
```toml
[return]
contract = "harwness.return.execution-summary@1"
```
Jeder eingebaute Agent braucht einen Rückgabevertrag — keine Rohausgaben an
den Aufraggeber, sondern eine strukturierte Zusammenfassung.

## Ablageorte
- **Projekt-Scope**: `<projekt>/.harw/agents/<name>/definition.toml` — für
  dieses Repository sichtbar, gehört ins Projekt.
- **Profil-Scope**: `~/.harw/profiles/<profil>/agents/<name>/definition.toml`
  (siehe `harw-home/src/scaffold.rs`) — persönlich, profilweit, nie im
  Projekt-Repository.
- **Legacy**: die flache Form `<dir>/<name>.toml` wird nur noch gelesen (mit
  Warnung, siehe `config_agents.rs`), nie mehr geschrieben — neue
  Definitionen landen immer im Verzeichnisformat oben.

## Häufige Fehler
- `admitted` weicht vom Registry-Profil der Rolle ab (zu viel oder zu
  wenig) — bricht `tool_admission_coverage.rs`.
- Schreibende oder ausführende Werkzeuge ohne begründetes, engeres
  Registry-Profil zulassen, statt `RegistryProfile::Full` zu vermeiden.
- Fehlender `[return].contract` — ein Kind ohne Rückgabevertrag liefert
  unstrukturierten Text zurück.
- `specialization` weicht vom Dateinamen/Rollennamen ab.

## Kurzes Worker-Beispiel
```toml
schema = "harwness.agent/v1"
id = "harwness.agent.my-read-only-worker@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "my-read-only-worker"

name = "My Read-Only Worker"
description = "Liest und fasst zusammen, ohne zu schreiben."

[tools]
admitted = ["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep", "doc.read_pdf"]
forbidden = ["fs.write", "shell.exec"]

[spawn]
max_depth = 0

[spawn.budget]
max_tokens = 40000
max_tool_calls = 30
max_wall_secs = 120

[return]
contract = "harwness.return.execution-summary@1"
```

## Rechte-Algebra beim Schreiben (Nachtrag K3)
`agent-steward` prüft jeden Entwurf zweifach, bevor er schreibt: (a) gegen
die **Urheber-Decke** (effektive Rechte, Tiefe, Budget, Effort des
Aufraggebers) — ein Delta hier ist eine harte Ablehnung, niemand verleiht
mehr, als er selbst hat; (b) gegen die **Basisrolle** (`profile_for_role`
plus eingebaute Definition der Zielrolle). Daraus ergibt sich `review_level`:
`uia` (neue UIA) immer `user_required`; mehr Rechte als die Basisrolle
(aber ≤ Urheber) ebenfalls `user_required`; sonst `uia` (reine
UIA-Sichtprüfung); auftragsgebundene Agenten (`scope = "run"`) `none`.
`proposal.json` trägt beide Deltas, den Diff, das Validierungsergebnis,
`review_level`, `created_at` und `expires_at` (+7 Tage). Ein abgelaufener
Vorschlag wird beim Commit abgelehnt; der Commit validiert und berechnet
beide Deltas erneut — diesmal gegen die Decke des **committenden**
Aufrufers. `user_required` braucht `user_confirmed: true`, gesetzt
ausschließlich über den Freigabe-Fluss, nie als bloßes Modell-Argument.

## UIA erstellen (Persönlichkeitsprofil, Identität, Nutzerkontext)
Eine neue UIA wird **nie** stillschweigend erzeugt oder aktiviert
(`harw-cli/src/uia_bootstrap.rs`) — sie entsteht nur als sichtbarer,
bestätigter Vorschlag. Ablauf: die UIA erfragt mit dem Nutzer zusammen
Persönlichkeit, Identität und Nutzerkontext; `agent-steward` setzt das über
das Werkzeug `agents.write_uia` um.

### Bündel-Aufbau
Ein UIA-Bündel liegt **ausschließlich im Profil-Scope**
(`~/.harw/profiles/<profil>/agents/<dir>/`) — persönliche Dateien gehören
nie in ein Projekt-Repository:
- `definition.toml`: `role = "user-interface"` plus Identität (`id`,
  `version`, `name`, `description`, `specialization`) — wie oben, nur mit
  dieser Rolle statt `worker`.
- `agent.toml`: dieselben Kernfelder (`name`, `role`, `description`) als
  Agentenmetadaten neben der Definition (legacy, siehe
  `agent_definition_tools.rs::build_agent_toml`/`commit_uia_bundle`) — trägt
  seit Plan R9 **kein** `identity`-Feld mehr.
- `Personality.md`: Ton, Persönlichkeit, Antwortverhalten — wie die UIA
  klingen und reagieren soll.
- `USER.md` (freiwillig): Nutzerkontext, üblicherweise mit einer
  `Name: …`-Zeile, die `harw-config::loader::load_uia_user_name` liest.
- `identity.md` (freiwillig, über `identity_md` von `agents.write_uia`): wer
  der Agent selbst ist (Name, Herkunft, Abgrenzung zu anderen UIAs) — der
  Loader (`harw-config/src/loader.rs::load_uia_identity`, aufgerufen aus
  `load_uia_personalization`) liest sie tatsächlich; ohne `identity_md`
  entfällt die Datei ersatzlos, kein Fehler.

### Aktivierung nur durch den Nutzer
`agents.write_uia` schreibt das Bündel atomar je Datei mit `0600`
(Unix-Dateirechten) und **aktiviert nie** — `active_uia_definition` bleibt
unverändert. Das Ergebnis nennt dem Nutzer, wie er die neue UIA von Hand
aktivieren kann. Keine Geheimnisse in den Dateien (Heuristik prüft u. a.
auf `sk-`, `-----BEGIN`, `api_key =` und lehnt sonst ab).
