# C-CFG – W3 Verträge: `[network]`/`[browser]`/`[dod]`/`[web]`-Config-Sektionen

Rolle: Coding-Agent (Opus), Owned files laut Brief: `harw-config/src/{browser_toml,
network_toml,dod_toml,web_toml}.rs` (alle neu), `harw-config/src/discovery.rs`
(nur neue Felder + Merge), diese Ledger-Datei. `harw-config/src/lib.rs`
(`mod`/`pub use`) ist **nicht** owned — exakte Folgearbeit siehe unten.
`harw-config/src/harness_config.rs` ist ebenfalls **nicht** owned; siehe
„Architekturentscheidung“ unten, warum die Lösung ohne Zugriff darauf
auskommt.

Build-Policy eingehalten: kein `cargo check/build/test/clippy/run/add`, kein
`rustc`/`rust-analyzer`, keine `git`-Schreibbefehle. Ausgeführt:
`cargo metadata --offline --no-deps --format-version 1 --manifest-path
harw-config/Cargo.toml` (schlägt fehl, weil der Workspace-Member
`harw-egress` noch kein `Cargo.toml` hat — nicht in meiner Zuständigkeit,
kein Blocker für diesen Auftrag). Format von Hand geprüft (`awk 'length >
100'` über alle vier neuen Dateien und den geänderten `discovery.rs`-Bereich
— keine Überlänge außer bereits vorbestehenden Zeilen in fremden Tests, die
ich nicht angefasst habe).

## Dateien

- `harw-config/src/network_toml.rs` (neu) — `NetworkSection`.
- `harw-config/src/browser_toml.rs` (neu) — `BrowserSection`.
- `harw-config/src/dod_toml.rs` (neu) — `DodSection`.
- `harw-config/src/web_toml.rs` (neu) — `WebSection`.
- `harw-config/src/discovery.rs` — vier neue Felder auf `ResolvedConfig`
  (`network`, `browser`, `dod`, `web`), unabhängiges Parsen dieser vier
  Top-Level-Tabellen aus `config.toml` (`extract_section`,
  `strip_new_sections`, `NEW_SECTION_KEYS`), Merge-Funktionen für den nicht
  vertrauten Repo-Layer (`merge_restricted_network`,
  `merge_restricted_browser`, `merge_restricted_dod`, gemeinsame
  `field_present`-Präsenzprüfung), neue Tests. `apply_restricted_layer`s
  Signatur geändert von `&mut HarnessConfig` auf `&mut ResolvedConfig`
  (Aufrufstelle in `discover_config_with_restricted` mitgezogen).

## Architekturentscheidung: keine Änderung an `harness_config.rs`

Die vier neuen Sektionen sind **nicht** Felder von `HarnessConfig`. Grund:
`harness_config.rs` steht nicht in meiner Dateizuständigkeit, aber
`HarnessConfig` trägt `#[serde(deny_unknown_fields)]` — jede `config.toml`
mit `[network]`/`[browser]`/`[dod]`/`[web]` würde beim Parsen zu
`HarnessConfig` sonst mit „unknown field“ scheitern, in **jedem** Layer
(vertraut und eingeschränkt), nicht nur bei mir.

Lösung, vollständig innerhalb von `discovery.rs` (mein Owned file): Pro
Layer wird `config.toml` zuerst als `toml::Value` geparst (`fields`). Daraus
werden `[network]`/`[browser]`/`[dod]`/`[web]` unabhängig extrahiert
(`extract_section::<T>(&fields, "key")`, nutzt `toml::Value::try_into`) und
in die neuen `ResolvedConfig`-Felder geschrieben. Anschließend werden genau
diese vier Top-Level-Keys aus einer Kopie von `fields` entfernt
(`strip_new_sections`, nutzt `toml::Table::remove`), bevor der Rest wie
zuvor gegen `HarnessConfig` deserialisiert wird
(`harness_fields.try_into::<HarnessConfig>()` statt `toml::from_str`).
`HarnessConfig` selbst bleibt unverändert und sieht diese vier Sektionen nie.

Bewusster Kompromiss: Fehler beim Parsen von `HarnessConfig` laufen jetzt
über `Value::try_into` statt `toml::from_str`, was Zeilen-/Spalten-Angaben in
`ConfigError::TomlParse`-Meldungen für den `HarnessConfig`-Teil verlieren
kann (die vier neuen Sektionen selbst behalten ihre eigenen, direkten
`toml::from_str`-Fehlermeldungen in ihren `#[cfg(test)]`-Modulen, da dort
`toml::from_str::<NetworkSection>(...)` etc. direkt aufgerufen wird, nicht
über `discovery.rs`). Sobald `harness_config.rs` eigene Felder für diese vier
Sektionen bekommt (siehe Folgearbeit), kann dieser Umweg entfernt und wieder
direkt `toml::from_str::<HarnessConfig>` verwendet werden.

Merge-Semantik zwischen **vertrauten** Layern (Home → Profil): Ist der Key im
aktuellen Layer vorhanden, ersetzt er den bisherigen Wert vollständig
(gleiche Wholesale-Ersetzung wie für `cfg: HarnessConfig` selbst). Fehlt der
Key, bleibt der Wert des vorherigen vertrauten Layers erhalten
("Home-Layer setzt"; Test
`trusted_layer_sets_new_sections_and_a_later_layer_without_them_carries_forward`).

## Merge-Regeln für den nicht vertrauten Repo-Layer (`apply_restricted_layer`)

| Sektion.Feld | Regel | Begründung |
|---|---|---|
| `network.allow_hosts` | Schnittmenge mit vertrautem Stand | mehr Hosts nie übernehmbar |
| `network.researcher_web_hosts` | Schnittmenge | dito |
| `network.allow_private` | logisches UND (nur Richtung `false`) | `false` ist der sichere Default |
| `browser.enabled` | logisches UND (nur `true`→`false`) | „enabled nur true→false“ lt. Brief |
| `browser.allowed_origins` | Schnittmenge | mehr Origins nie übernehmbar |
| `browser.max_actions` | Minimum, `0` aus Repo nie übernommen (`min_positive`) | Obergrenze, `0` könnte als „unbegrenzt“ gelesen werden |
| `browser.geckodriver_path`/`geckodriver_sha256` | **nie** übernommen | Umlenkung auf fremdes Binary wäre Rechteausweitung, kein Verengen |
| `dod.kill_requires_human` | **nie** aus Repo übernommen | bleibt beim vertrauten, bereits validierten `true`; zusätzlich erzwingt `DodSection::validate` `true` in jeder Layer unabhängig |
| `dod.auto_freeze` | logisches ODER (nur Richtung `true`) | `true` ist der sichere Default (mehr Automatik = mehr Schutz) |
| `dod.proof_key_dir` | **nie** übernommen | Umlenkung auf fremdes Schlüsselverzeichnis wäre Rechteausweitung |
| `dod.allowed_cgroup_prefixes` | Schnittmenge | mehr Präfixe nie übernehmbar |
| `[web]` (alle drei Felder) | **nie** berücksichtigt, auch nicht verengend | Brief: „Web-Bind nicht vom Repo“ — `apply_restricted_layer` ruft für `web` weder `extract_section` noch eine Merge-Funktion auf |

Alle Regeln sind Erweiterungen der bestehenden `merge_restricted_harness`-
Mechanik aus W1-06a (`present`-Präsenzprüfung über den geparsten
`toml::Value`, `min_positive` für Obergrenzen) — hier als eigenständige
Funktionen `merge_restricted_network`/`_browser`/`_dod` mit gemeinsamer
`field_present`-Hilfsfunktion (identische Logik zur lokalen `present`-Closure
in `merge_restricted_harness`, die unverändert bleibt).

„Untrusted Repo liefert gar nichts“ (W1-06a) bleibt unverändert gültig: Diese
gesamte Merge-Logik läuft nur, wenn der Aufrufer `restricted_repo: Some(..)`
übergibt (typischerweise `harw_home::LayerReport::untrusted_repo` bei Status
`Trusted`-Ausschluss); ein tatsächlich `Untrusted`/`Changed`-Repo wird laut
`config_layers_report` (W1-06a) gar nicht als `restricted_repo` weitergereicht
und trägt nichts bei.

## Öffentliche API (neu, exakt)

```rust
// harw_config::network_toml
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    pub allow_hosts: Vec<String>,        // default: []
    pub allow_private: bool,             // default: false
    pub researcher_web_hosts: Vec<String>, // default: []
}
impl NetworkSection { pub fn validate(&self) -> Result<(), String>; }

// harw_config::browser_toml
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSection {
    pub enabled: bool,                        // default: false
    pub allowed_origins: Vec<String>,         // default: []
    pub max_actions: u32,                     // default: 20
    pub geckodriver_path: Option<PathBuf>,    // default: None
    pub geckodriver_sha256: Option<String>,   // default: None
}
impl Default for BrowserSection { .. }
impl BrowserSection { pub fn validate(&self) -> Result<(), String>; }

// harw_config::dod_toml
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DodSection {
    pub kill_requires_human: bool,             // default: true, validate() erzwingt true
    pub auto_freeze: bool,                     // default: true
    pub proof_key_dir: Option<PathBuf>,        // default: None
    pub allowed_cgroup_prefixes: Vec<String>,  // default: []
}
impl Default for DodSection { .. }
impl DodSection { pub fn validate(&self) -> Result<(), String>; }

// harw_config::web_toml
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSection {
    pub bind: String,          // default: "127.0.0.1", validate() erzwingt 127.0.0.1|::1
    pub port: u16,             // default: 0
    pub token_ttl_secs: u64,   // default: 900
}
impl Default for WebSection { .. }
impl WebSection { pub fn validate(&self) -> Result<(), String>; }

// harw_config::discovery — neue/geänderte Signaturen
pub struct ResolvedConfig {
    // .. bestehende Felder unverändert ..
    pub network: NetworkSection,
    pub browser: BrowserSection,
    pub dod: DodSection,
    pub web: WebSection,
}
// apply_restricted_layer: Signatur geändert (war `&mut HarnessConfig`)
fn apply_restricted_layer(base: &Path, resolved: &mut ResolvedConfig) -> ConfigResult<()>;
// neu, privat:
const NEW_SECTION_KEYS: [&str; 4] = ["network", "browser", "dod", "web"];
fn strip_new_sections(value: &mut toml::Value);
fn extract_section<T: serde::de::DeserializeOwned>(fields: &toml::Value, key: &str) -> ConfigResult<Option<T>>;
fn field_present(fields: &toml::Value, path: &[&str]) -> bool;
fn merge_restricted_network(trusted: &mut NetworkSection, restricted: &NetworkSection, fields: &toml::Value);
fn merge_restricted_browser(trusted: &mut BrowserSection, restricted: &BrowserSection, fields: &toml::Value);
fn merge_restricted_dod(trusted: &mut DodSection, restricted: &DodSection, fields: &toml::Value);
```

`discover_config`/`discover_config_with_restricted` behalten ihre bisherigen
Signaturen bei; `merge_restricted_harness`/`min_positive`/
`read_restricted_file`/`MAX_RESTRICTED_CONFIG_BYTES` sind unverändert.

## API-Belege (per Lesen verifiziert)

- `toml-0.8.23/src/value.rs:64` `Value::try_into<'de, T>(self) -> Result<T, crate::de::Error>
  where T: de::Deserialize<'de>`.
- `toml-0.8.23/src/value.rs:79` `Value::get<I: Index>(&self, index: I) -> Option<&Value>`
  (bereits von W1-06a/bestehendem Code genutzt, hier für `field_present`/
  `extract_section` wiederverwendet).
- `toml-0.8.23/src/value.rs:23-38` `enum Value { .., Table(Table) }`.
- `toml-0.8.23/src/table.rs:13` `pub type Table = Map<String, Value>`.
- `toml-0.8.23/src/map.rs:146` `Map::remove<Q>(&mut self, key: &Q) -> Option<Value>`.
- `toml-0.8.23/src/de.rs:50` `pub struct Error` (⇒ `toml::de::Error`, `de` als
  `pub mod de;` in `toml-0.8.23/src/lib.rs:154`).
- `harw-config/Cargo.toml:14` `toml = "0.8"` (nicht die Workspace-Version
  `1.1.3` — wie bereits in `docs/remediation/ledger/W1/W1-06a.md` für
  `harw-config` dokumentiert; alle obigen Zeilenangaben beziehen sich auf
  `toml-0.8.23`, die im `Cargo.lock` vorhandene 0.8-Version).
- `serde::de::DeserializeOwned`: keine lokale Kopie von `serde` 1.x im
  `~/.cargo`-Registry-Cache gefunden (nur `toml`, `blake3` u. Ä. sind
  vendored); Standard-Trait aus `serde::de`, bereits transitiv über
  `serde = { version = "1", features = ["derive"] }`
  (`harw-config/Cargo.toml:11`) verfügbar — kein neuer Dep-Request nötig.
- `harw-config/src/discovery.rs` (vor dieser Änderung, W1-06a):
  `merge_restricted_harness`/`min_positive`/`apply_restricted_layer`-Muster,
  hier für die vier neuen Sektionen repliziert statt verändert.

## Tests (neu)

Je Sektion (`network_toml.rs`, `browser_toml.rs`, `dod_toml.rs`,
`web_toml.rs`): `test_*_section_defaults_from_empty_toml`,
`test_*_section_full_toml_round_trip` (inkl. `toml::to_string` +
Re-Parse-Vergleich), `test_*_section_rejects_unknown_field`, plus
`validate()`-Tests je Invariante (siehe Tabelle):

- `network_toml.rs`: Default, Round-Trip, unbekanntes Feld,
  `validate` akzeptiert Default, lehnt leeren/Schema-behafteten
  Host-Eintrag ab (je einmal `allow_hosts`/`researcher_web_hosts`).
- `browser_toml.rs`: Default, Round-Trip (inkl. gepinntem `geckodriver_*`),
  unbekanntes Feld, `validate` akzeptiert Default sowie vollständig
  gepinnte+freigeschaltete Sektion, lehnt ab: `enabled` ohne Origins,
  Origin ohne Schema, `max_actions = 0`, `geckodriver_path` ohne `_sha256`
  und umgekehrt, fehlerhaften/großgeschriebenen SHA-256.
- `dod_toml.rs`: Default, Round-Trip, unbekanntes Feld, `validate`
  akzeptiert Default sowie `auto_freeze = false` (bei weiterhin
  `kill_requires_human = true`), lehnt **`kill_requires_human = false` ab**
  (die im Brief geforderte Pflichtprobe) und leeren `allowed_cgroup_prefixes`-
  Eintrag.
- `web_toml.rs`: Default, Round-Trip, unbekanntes Feld, `validate`
  akzeptiert Default sowie `::1`, lehnt `0.0.0.0` und `token_ttl_secs = 0` ab.

In `discovery.rs` (Merge-Verhalten, spiegelt die W1-06a-Teststruktur):
- `trusted_layer_sets_new_sections_and_a_later_layer_without_them_carries_forward`
  — Home setzt alle vier Sektionen vollständig; ein zweiter vertrauter Layer,
  der nur `[logging]` ändert, lässt sie unverändert ("Home-Layer setzt").
- `restricted_repo_narrows_network_browser_and_dod_but_never_web` — Home mit
  großzügigen, aber gültigen Werten; Repo versucht überall zu verengen
  *und* an einer Stelle zu erweitern (`dod.auto_freeze: true`, was hier die
  sichere Richtung ist und daher durchschlägt); prüft alle Ziel-Werte plus
  `validate()` auf dem Ergebnis jeder Sektion; `[web]` bleibt exakt beim
  Home-Wert trotz abweichendem Repo-`[web]` (0.0.0.0:80, Riesen-TTL).
- `restricted_repo_cannot_widen_network_browser_or_dod` — Home bleibt bei
  den restriktiven Defaults (leere `config.toml`), Repo versucht in jede
  Richtung zu erweitern (Hosts, `allow_private`, `enabled`, Origins,
  `max_actions`, `auto_freeze` bereits `true`); Ergebnis bleibt exakt bei den
  Home-Defaults.

## Folgearbeit (nicht owned, exakte Zeilenangaben)

### `harw-config/src/lib.rs` (Stand vor dieser Änderung, wie gelesen)

```
1  #![forbid(unsafe_code)]
2
3  pub mod agent_toml;
4  pub mod auth_toml;
5  pub mod channel_toml;
6  pub mod discovery;
7  pub mod dotenv;
8  pub mod error;
9  pub mod harness_config;
10 pub mod loader;
11 pub mod mcp_toml;
12 pub mod mode_toml;
13 pub mod model_toml;
14 pub mod plan_toml;
15 pub mod plugin_toml;
16 pub mod provider_toml;
17 pub mod research_toml;
18 pub mod skill_toml;
```

Benötigte Einfügungen (alphabetisch, wie der bestehende Block sortiert ist):

- nach Zeile 4 (`pub mod auth_toml;`): `pub mod browser_toml;`
- nach Zeile 6 (`pub mod discovery;`): `pub mod dod_toml;`
- nach Zeile 13 (`pub mod model_toml;`): `pub mod network_toml;`
- nach Zeile 18 (`pub mod skill_toml;`): `pub mod web_toml;`

Und im `pub use`-Block (Zeilen 20ff., alphabetisch nach Modulnamen sortiert):
`pub use browser_toml::BrowserSection;` (nach der `auth_toml`-Zeile),
`pub use dod_toml::DodSection;` (nach der `discovery`-Zeile-Gruppe),
`pub use network_toml::NetworkSection;` (nach der `model_toml`-Zeile),
`pub use web_toml::WebSection;` (nach der `skill_toml`-Zeile).

Ohne diese vier `pub mod`-Zeilen lösen die in `discovery.rs` neu
hinzugefügten `use crate::{browser_toml::BrowserSection, dod_toml::DodSection,
network_toml::NetworkSection, web_toml::WebSection};`-Imports nicht auf —
das ist der einzige noch offene Compile-Schritt für diesen Auftrag.

### `harw-config/src/harness_config.rs` (optional, nicht erforderlich)

Nicht nötig für Korrektheit (siehe „Architekturentscheidung“ oben), aber
optional für bessere Fehlermeldungen: Sobald ein Agent `harness_config.rs`
besitzt, könnte er `pub network: NetworkSection`, `pub browser:
BrowserSection`, `pub dod: DodSection`, `pub web: WebSection` als Felder von
`HarnessConfig` ergänzen (jeweils `#[serde(default)]`) und danach in
`discovery.rs` `extract_section`/`strip_new_sections`/`NEW_SECTION_KEYS`
sowie die separaten `ResolvedConfig`-Felder wieder entfernen (dann liest
`cfg.network` etc. direkt aus `HarnessConfig`). Bis dahin bleibt die
aktuelle, vollständig funktionsfähige Lösung in `discovery.rs` bestehen.

### Konsumenten (spätere Wellen, nicht Teil dieses Auftrags)

- W4a/W5 (`harw-egress` `C-EGRESS`/`N-EGRESS`, `harw-tool-web` `N-WEB`,
  `harw-registry-defaults` `RD`): `ResolvedConfig.network` konsumieren.
- W5 (`harw-browser`/`harw-tool-browser` `C-BROWSER`/`B-TOOL`,
  `harw-browser-thirtyfour` `B-ADAPT`): `ResolvedConfig.browser` konsumieren,
  insbesondere `geckodriver_path`/`geckodriver_sha256`-Pinning vor Prozessstart
  prüfen.
- W5 (`harw-dod-escalate`/`harw-escalator` `D-ESC`, `harw-dod-warden`/
  `harw-warden` `D-WARDEN`): `ResolvedConfig.dod` konsumieren,
  `kill_requires_human` vor jeder Kill-Eskalation erneut prüfen (Config allein
  ist keine Durchsetzung).
- W5 (`harw-web` `WB-SRV`/`WB-COMP`): `ResolvedConfig.web` konsumieren,
  `bind`/`port` für den TCP-Listener, `token_ttl_secs` für das
  Launch-Token.
- W2B-01 (Runtime mit Trust-Bericht, aus W1-06a-Folgearbeit): sobald
  `config_layers_report` + `discover_config_with_restricted` produktiv
  verdrahtet sind, gelten die hier definierten Repo-Verengungsregeln
  automatisch mit.

## Offene Annahmen / Design-Entscheidungen (dokumentiert, nicht im Brief spezifiziert)

- `browser.max_actions` Default `20` — kleiner, sicherer Wert, damit eine
  außer Kontrolle geratene Session begrenzt ist, bevor `harw-tool-browser`
  (W5 `B-TOOL`) eigene Journal-/Zeitlimits durchsetzt; `validate()` lehnt `0`
  ab (Deaktivierung erfolgt über `enabled = false`, nicht über `max_actions
  = 0`).
- `browser.validate()` verlangt bei `enabled = true` mindestens eine Origin
  in `allowed_origins` — ein freigeschaltetes, aber gänzlich origin-loses
  Werkzeug wäre nutzlos und ein Footgun.
- `browser.geckodriver_path`/`geckodriver_sha256` müssen laut `validate()`
  zusammen gesetzt sein (beide oder keins) — Pinning ohne Hash bzw. Hash ohne
  Pfad wäre kein Pinning.
- `web.token_ttl_secs` Default `900` (15 min) — im Plan nicht beziffert;
  kurz genug für ein Launch-Token, lang genug für eine Web-UI-Session.
- `web.bind` Default `127.0.0.1` (nicht `::1`) — gängigster Loopback, IPv6
  bleibt über explizite Konfiguration erreichbar.
- `dod.auto_freeze`-Repo-Merge als logisches ODER (nur Richtung `true`)
  interpretiert „bool nur Richtung sicher“ so, dass `true` (mehr Automatik,
  mehr Schutz) die sichere Richtung ist — anders als `network.allow_private`
  oder `browser.enabled`, wo `false` sicher ist. Begründung in der
  Merge-Tabelle oben.
- `network`/`browser`-Hostlisten-Validierung: `allow_hosts`/
  `researcher_web_hosts` erwarten reine Hostnamen (kein Schema, analog
  `research.network_allow_hosts`), `browser.allowed_origins` erwartet
  vollständige Origins **mit** Schema (Web-Plattform-Semantik von „Origin“) —
  bewusst unterschiedliche Validierung, in den Moduldocs je Feld erklärt.

## BLOCKED-Bewertung

Nicht blockiert: Die anfängliche Unklarheit (Sektionen müssten eigentlich
Felder von `HarnessConfig` sein, aber `harness_config.rs` ist nicht owned)
wurde durch eine Architekturentscheidung aufgelöst, die vollständig innerhalb
der eigenen Dateizuständigkeit (`discovery.rs`) bleibt und keine Änderung an
`harness_config.rs` voraussetzt. Der einzige verbleibende Schritt außerhalb
dieser Zuständigkeit ist die in „Folgearbeit“ exakt benannte
`lib.rs`-Verdrahtung (vier `pub mod` + vier `pub use`-Zeilen).
