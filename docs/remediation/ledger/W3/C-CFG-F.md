# C-CFG-F – Folgearbeit `harw-config/src/lib.rs` (Verdrahtung C-CFG)

Rolle: Coding-Agent, Owned files laut Brief: `harw-config/src/lib.rs`, diese
Ledger-Datei.

Build-Policy eingehalten: kein `cargo check/build/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`, keine `git`-Schreibbefehle. Ausgeführt:
`cargo metadata --offline --no-deps --format-version 1 --manifest-path
harw-config/Cargo.toml` — Exit-Code 0 (das in `docs/remediation/ledger/W3/
C-CFG.md` dokumentierte Problem mit fehlendem `harw-egress/Cargo.toml` ist
zum Zeitpunkt dieses Auftrags nicht mehr vorhanden; kein Blocker).

## Auftrag

Laut `docs/remediation/ledger/W3/C-CFG.md`, Abschnitt „Folgearbeit“, fehlten
in `harw-config/src/lib.rs` die vier `pub mod`- und vier `pub use`-Zeilen für
die von C-CFG neu angelegten Sektionsmodule
(`browser_toml`, `network_toml`, `dod_toml`, `web_toml`). Ohne sie lösten die
in `discovery.rs` bereits vorhandenen `use crate::{browser_toml::…, …}`-
Imports nicht auf.

## Vorher verifiziert

- Exakte Typnamen in den vier neuen Dateien gegen den Ledger-Anspruch
  geprüft (`grep -n "^pub struct" browser_toml.rs network_toml.rs
  dod_toml.rs web_toml.rs`):
  - `network_toml.rs:45` `pub struct NetworkSection`
  - `browser_toml.rs:48` `pub struct BrowserSection`
  - `dod_toml.rs:55` `pub struct DodSection`
  - `web_toml.rs:43` `pub struct WebSection`

  Stimmt exakt mit den im Ledger genannten Namen überein — keine Abweichung.
- `discovery.rs` referenziert alle vier Module bereits konsistent über
  `crate::…`-Pfade (Zeilen 3, 5, 11, 15):
  ```
  use crate::browser_toml::BrowserSection;
  use crate::dod_toml::DodSection;
  use crate::network_toml::NetworkSection;
  use crate::web_toml::WebSection;
  ```
  Keine abweichenden Pfade, keine Anpassung an `discovery.rs` nötig.

## Änderung an `harw-config/src/lib.rs`

`pub mod`-Block (alphabetisch einsortiert, bestehende Reihenfolge
beibehalten):

- `pub mod browser_toml;` nach `pub mod auth_toml;` eingefügt.
- `pub mod dod_toml;` nach `pub mod discovery;` eingefügt.
- `pub mod network_toml;` nach `pub mod model_toml;` eingefügt.
- `pub mod web_toml;` nach `pub mod skill_toml;` eingefügt (neue letzte
  Zeile des Blocks).

`pub use`-Block (alphabetisch nach Modulnamen, exakt wie im Ledger
spezifiziert):

- `pub use browser_toml::BrowserSection;` nach der `auth_toml`-Zeile.
- `pub use dod_toml::DodSection;` nach der `discovery`-Gruppe.
- `pub use network_toml::NetworkSection;` nach der `model_toml`-Zeile.
- `pub use web_toml::WebSection;` nach der `skill_toml`-Zeile (neue letzte
  Zeile des Blocks).

Resultierender Datei-Kopf (`harw-config/src/lib.rs`, vollständig):

```rust
#![forbid(unsafe_code)]

pub mod agent_toml;
pub mod auth_toml;
pub mod browser_toml;
pub mod channel_toml;
pub mod discovery;
pub mod dod_toml;
pub mod dotenv;
pub mod error;
pub mod harness_config;
pub mod loader;
pub mod mcp_toml;
pub mod mode_toml;
pub mod model_toml;
pub mod network_toml;
pub mod plan_toml;
pub mod plugin_toml;
pub mod provider_toml;
pub mod research_toml;
pub mod skill_toml;
pub mod web_toml;

pub use agent_toml::{AgentSuggestionsToml, AgentToml};
pub use auth_toml::{AuthConfig, CredentialEntry, KekConfig, KekProvenance, SecretRef};
pub use browser_toml::BrowserSection;
pub use channel_toml::{ChannelFileToml, ChannelSectionToml, ChannelToml, TelegramChannelToml};
pub use discovery::{
    HasName, ResolvedConfig, default_config_layers, discover_config,
    discover_config_with_restricted,
};
pub use dod_toml::DodSection;
pub use dotenv::{
    check_dotenv_permissions, load_dotenv, load_env_layer, parse_dotenv_text, resolve_env_ref,
};
pub use error::{ConfigError, ConfigResult};
pub use harness_config::{
    HarnessConfig, LoggingSection, McpJobCapabilityToml, McpListenerSection, McpPrincipalToml,
    PolicySection, SessionSection, TuiSection,
};
pub use loader::{load_skill_instructions, load_system_prompt};
pub use mcp_toml::{McpServerToml, McpTransportToml};
pub use mode_toml::ModeSection;
pub use model_toml::{ModelCapabilitiesToml, ModelToml};
pub use network_toml::NetworkSection;
pub use plan_toml::{PlanSection, ToolsSection};
pub use plugin_toml::{PluginCapabilitiesToml, PluginToml};
pub use provider_toml::{OriginAllowlistToml, ProviderToml};
pub use research_toml::ResearchSection;
pub use skill_toml::SkillToml;
pub use web_toml::WebSection;
```

Keine sonstigen Zeilen verändert; kein anderes Modul/Import angefasst.

## Prüfung Pfadkonsistenz

- Alle vier neuen `pub mod`-Deklarationen zeigen auf die bereits
  existierenden Dateien `harw-config/src/{browser_toml,network_toml,
  dod_toml,web_toml}.rs` (Standard-Modulpfad-Konvention, keine `#[path]`-
  Attribute nötig oder vorhanden).
- `discovery.rs`s `use crate::…`-Imports lösen jetzt auf, da die Module in
  `lib.rs` öffentlich deklariert sind.
- Keine Namenskollision mit bestehenden `pub use`-Re-Exports (`BrowserSection`,
  `NetworkSection`, `DodSection`, `WebSection` waren vorher nicht vergeben).

## Nicht owned / nicht angefasst

- `harw-config/src/{browser_toml,network_toml,dod_toml,web_toml}.rs` selbst
  (Inhalt stammt vollständig aus C-CFG, nur gelesen zur Typnamen-Verifikation).
- `harw-config/src/discovery.rs` (nur gelesen zur Pfad-Konsistenzprüfung,
  keine Änderung nötig).
- `harw-config/src/harness_config.rs` — laut C-CFG-Architekturentscheidung
  weiterhin unverändert, keine der vier Sektionen wird dort als Feld
  aufgenommen.

## BLOCKED-Bewertung

Nicht blockiert. Brief war vollständig und deckungsgleich mit dem Ledger-
Abschnitt „Folgearbeit“ in `C-CFG.md`; alle vier `pub mod`- und `pub use`-
Zeilen exakt wie dort spezifiziert eingefügt, Typnamen vorab gegen die
Quelldateien verifiziert, `discovery.rs`-Importpfade bestätigt konsistent.
