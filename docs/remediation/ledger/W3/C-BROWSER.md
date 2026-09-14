# C-BROWSER – Browser-Policy: Same-Origin, Upload/CustomScript entfernt, Limits, Serde-Bypass zu (F-007–F-010, F-114, F-115)

Rolle: focused-coding-task (Opus), Welle W3 Teil B. Kein `cargo build/check/test/clippy/run/add`, kein
`make`/`rustc`/`rust-analyzer`/`rustfmt` (rustfmt fehlt auf dem Host), keine `git`-Schreibbefehle.
Ausgeführt: Lesen, `grep`, `cargo metadata --offline --no-deps --format-version 1` (OK). Verifikation durch Lesen.

Pflichtlektüre: `docs/remediation/AGENT-BRIEF.md`; Plan `eventual-wandering-pebble.md` Teil B (W3-Tabelle C-BROWSER,
W5 B-ADAPT/B-TOOL); Register `x-findings-register-w1-w3.md:88-91` (F-007…F-010), `:222-223` (F-114, F-115);
Detailbericht `w1-browser-wardenproto.md:40-67,176-197,379,406`. Verifiziert per Registry-Quelle:
`serde_derive-1.0.228/src/de/struct_.rs:285/580/682` (Container-`deny_unknown_fields` gilt auch für
Struct-Varianten externer Enums), `url-2.5.8/src/lib.rs:1162` (`host_str` liefert IPv6 **mit** Klammern → Policy nutzt
`Url::host()`), `harw-sandbox/src/egress.rs:327` (`host_matches_suffix`).

## Geänderte Dateien

| Datei | Änderung |
|---|---|
| `harw-browser/src/policy.rs` | Neu: `OriginRule`/`OriginScheme` (Same-Origin), `OriginPolicy` mit privaten Feldern + Validierung, erweitertes Private-Veto, `BrowserLimits` + Konstanten, `ProfilePolicy::validate`, `Viewport::validate`, `OpenBrowserRequest.authentication_origins`/`.limits` + `validate`/`check_navigation_target`/`check_observed_location`; `deny_unknown_fields` überall; 29 Tests |
| `harw-browser/src/action.rs` | `BrowserAction::Upload` **entfernt**; `deny_unknown_fields`; `BrowserAction::validate`, `navigation_target`, `validate_target`, `validate_selector`, `ActionRequest::validate`, `ActionBudget`; Konstanten `MAX_KEY_BYTES`, `MAX_SCROLL_DELTA`; 20 Tests |
| `harw-browser/src/wait.rs` | `WaitCondition::CustomScript` **entfernt**; `deny_unknown_fields`; `WaitCondition::validate`; `WaitTimeout` jetzt `u64`-Millis (Serde = Integer, 1..=`HARD_MAX_WAIT_MS`), `try_from_millis`, `millis`, `validate`; `MAX_SCRIPT_CHANNEL_BYTES`; 11 Tests |
| `harw-tool-browser/src/types.rs` | `OpenRequest` ohne Autoritätsfelder; `BrowserOpenGrant.limits`; `authorize` hält Auth-Origins getrennt; neues `PreparedBrowserRequest`; `PreparedBrowserCall` versiegelt (kein Serde, private Felder, `scope()`); `BrowserResourceScope` nur `Serialize`; `deny_unknown_fields` auf allen Request/Response-Typen; 13 Tests |

Keine Änderung an `Cargo.toml` (kein dep-request). `harw-sandbox::egress::host_matches_suffix` wird **nicht**
importiert: `harw-browser` ist L0-Vertragscrate ohne `harw-sandbox`-Abhängigkeit; die Semantik (ASCII-case-insensitiv,
ein Trailing-Dot, Label-Grenze, IP exakt) ist in `policy.rs::domain_matches` gespiegelt, plus Apex-Ausschluss für
Wildcard-Regeln.

## Geänderte Typen und Signaturen (exakt)

### `harw_browser::policy`

```rust
pub const MAX_ORIGIN_RULES: usize = 64;
pub const MAX_ORIGIN_RULE_BYTES: usize = 512;
pub const MAX_PROFILE_BINDING_BYTES: usize = 128;
pub const MAX_VIEWPORT_DIMENSION: u32 = 8_192;
pub const DEFAULT_MAX_ACTIONS_PER_SESSION: u32 = 200;   pub const HARD_MAX_ACTIONS_PER_SESSION: u32 = 5_000;
pub const DEFAULT_MAX_WAIT_MS: u64 = 30_000;            pub const HARD_MAX_WAIT_MS: u64 = 120_000;
pub const DEFAULT_MAX_TEXT_BYTES: usize = 4_096;        pub const HARD_MAX_TEXT_BYTES: usize = 65_536;
pub const DEFAULT_MAX_SELECTOR_BYTES: usize = 512;      pub const HARD_MAX_SELECTOR_BYTES: usize = 4_096;
pub const DEFAULT_MAX_SELECTOR_FALLBACKS: usize = 4;    pub const HARD_MAX_SELECTOR_FALLBACKS: usize = 16;
pub const DEFAULT_MAX_URL_BYTES: usize = 2_048;         pub const HARD_MAX_URL_BYTES: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub enum ProfilePolicy { Ephemeral, Persistent { binding: String } }
impl ProfilePolicy { pub fn validate(&self) -> Result<()>; }          // 1..=128 Bytes [A-Za-z0-9._-], nicht "."/".."

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct Viewport { pub width: u32, pub height: u32 }
impl Viewport { pub fn validate(&self) -> Result<()>; }               // 1..=8192

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub enum BiDiRequirement { Required, Preferred, NotRequired }          // unverändert bis auf Attribut

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OriginScheme { Http, Https }
impl OriginScheme { pub fn as_str(self) -> &'static str; pub fn default_port(self) -> u16; }

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)] #[serde(try_from = "String", into = "String")]
pub struct OriginRule { /* private: scheme, host (Domain lowercase ohne Trailing-Dot | IpAddr), port, include_subdomains */ }
impl OriginRule {
    pub fn parse(input: &str) -> Result<Self>;
    pub fn scheme(&self) -> OriginScheme;
    pub fn port(&self) -> u16;
    pub fn include_subdomains(&self) -> bool;
    pub fn matches(&self, url: &url::Url) -> bool;                    // ohne Private-Veto
}
impl Display for OriginRule; impl TryFrom<String> for OriginRule; impl From<OriginRule> for String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(try_from = OriginPolicyRepr /* deny_unknown_fields */)]
pub struct OriginPolicy { /* private: allow: Vec<OriginRule>, deny_private_networks: bool */ }
impl Default for OriginPolicy;                                          // leer + Veto an = deny-all
impl OriginPolicy {
    pub fn new(allow: Vec<OriginRule>, deny_private_networks: bool) -> Result<Self>;   // WAR: (Vec<String>, bool) -> Self
    pub fn from_origins<I, S>(origins: I, deny_private_networks: bool) -> Result<Self>
        where I: IntoIterator<Item = S>, S: AsRef<str>;
    pub fn rules(&self) -> &[OriginRule];
    pub fn origins(&self) -> Vec<String>;                             // kanonische Regeltexte
    pub fn deny_private_networks(&self) -> bool;                      // WAR: pub-Feld
    pub fn is_empty(&self) -> bool;
    #[must_use] pub fn with_private_network_veto(self) -> Self;       // nur verschärfen
    pub fn is_allowed(&self, url: &url::Url) -> bool;                 // Signatur gleich, Semantik neu
    pub fn check(&self, url: &url::Url) -> Result<()>;                // Error::OriginNotAllowed { origin: ascii-Origin }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(try_from = BrowserLimitsRepr /* deny_unknown_fields, Bereichsprüfung */)]
pub struct BrowserLimits { /* private: max_actions_per_session: u32, max_wait_ms: u64, max_text_bytes, max_selector_bytes,
                               max_selector_fallbacks, max_url_bytes: usize */ }
impl Default for BrowserLimits;                                         // DEFAULT_*
impl BrowserLimits {
    #[must_use] pub fn with_max_actions_per_session(self, u32) -> Self; // alle with_* klemmen auf 1..=HARD (Fallbacks 0..=HARD)
    #[must_use] pub fn with_max_wait_ms(self, u64) -> Self;
    #[must_use] pub fn with_max_text_bytes(self, usize) -> Self;
    #[must_use] pub fn with_max_selector_bytes(self, usize) -> Self;
    #[must_use] pub fn with_max_selector_fallbacks(self, usize) -> Self;
    #[must_use] pub fn with_max_url_bytes(self, usize) -> Self;
    pub fn max_actions_per_session(&self) -> u32; pub fn max_wait_ms(&self) -> u64;
    pub fn max_text_bytes(&self) -> usize; pub fn max_selector_bytes(&self) -> usize;
    pub fn max_selector_fallbacks(&self) -> usize; pub fn max_url_bytes(&self) -> usize;
    pub fn check_url_len(&self, url: &url::Url) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct OpenBrowserRequest {
    pub start_url: url::Url,
    pub headless: bool,
    pub profile: ProfilePolicy,
    pub bidi: BiDiRequirement,
    pub allowed_origins: OriginPolicy,
    pub authentication_origins: OriginPolicy,   // NEU (F-114): nur beobachtete Location, nie Navigationsziel/Start
    pub viewport: Option<Viewport>,
    pub limits: BrowserLimits,                  // NEU
}
impl OpenBrowserRequest {
    pub fn validate(&self) -> Result<()>;                                  // Start ∈ allowed_origins, Länge, Viewport, Binding
    pub fn check_navigation_target(&self, url: &url::Url) -> Result<()>;   // nur allowed_origins
    pub fn check_observed_location(&self, url: &url::Url) -> Result<()>;   // allowed ∪ authentication
}
```

Origin-Semantik: Regel = `http[s]://host[:port][/]` oder `http[s]://*.domain[:port][/]`; Scheme+Host+Port exakt;
Exakt-Regel matcht keine Subdomain; `*.` matcht strikte Subdomains, **nicht** den Apex; Wildcard auf IP oder
Ein-Label-Suffix (`*.com`) abgelehnt; Pfad/Query/Fragment/Userinfo/Großbuchstaben-Scheme abgelehnt; URLs mit
Credentials nie erlaubt; nur `http`/`https`. Private-Veto (vor Regeln, nicht übersteuerbar): IPv4 loopback, RFC1918,
link-local (169.254/16), unspecified/0/8, broadcast, multicast, documentation, CGNAT 100.64/10, 192.0.0/24, 198.18/15,
240/4; IPv6 loopback, unspecified, multicast, ULA fc00::/7, link-local fe80::/10, site-local fec0::/10, v4-mapped,
v4-compatible, NAT64 64:ff9b::/96 (eingebettete v4 klassifiziert); `localhost`, `*.localhost`, jeweils mit Trailing-Dot.
DNS-Namen, die auf private Adressen auflösen, bleiben Aufgabe der Egress-Schicht (N-EGRESS/B-ADAPT).

### `harw_browser::action`

```rust
pub const MAX_KEY_BYTES: usize = 64;
pub const MAX_SCROLL_DELTA: i32 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub enum BrowserAction {
    Click { target: Target }, Type { target: Target, text: String }, Clear { target: Target },
    Select { target: Target, value: String }, Focus { target: Target }, Hover { target: Target },
    Scroll { target: Option<Target>, x: i32, y: i32 }, KeyPress { target: Option<Target>, key: String },
    Drag { source: Target, destination: Target }, Submit { target: Target },
    Navigate { url: url::Url }, Back, Forward, Reload,
    // ENTFERNT: Upload { target: Target, file_path: String }
}
impl BrowserAction {
    pub fn validate(&self, limits: &BrowserLimits) -> Result<()>;   // Targets, Text/Value, Key, Scroll, Navigate-Scheme+Länge
    pub fn navigation_target(&self) -> Option<&url::Url>;
}
pub fn validate_target(target: &Target, limits: &BrowserLimits) -> Result<()>;
pub fn validate_selector(selector: &Selector, limits: &BrowserLimits) -> Result<()>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct ActionRequest { pub context_id, pub expected_revision, pub action }   // Felder unverändert
impl ActionRequest { /* new, with_expected_revision unverändert */ pub fn validate(&self, limits: &BrowserLimits) -> Result<()>; }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionBudget { /* private used, max */ }
impl ActionBudget { pub fn new(limits: &BrowserLimits) -> Self; pub fn try_consume(&mut self) -> Result<()>;
                    pub fn used(&self) -> u32; pub fn remaining(&self) -> u32; }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct ActionOutcome { /* unverändert */ }
```

### `harw_browser::wait`

```rust
pub const MAX_SCRIPT_CHANNEL_BYTES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub enum WaitCondition {
    ElementPresent(Target), ElementVisible(Target), ElementClickable(Target), ElementGone(Target),
    UrlMatches(String), TitleMatches(String), NavigationComplete, NetworkQuiescence { idle_ms: u64 },
    RequestObserved { path_contains: String }, LogMatches(String), ScriptMessage { channel: String }, DownloadComplete,
    // ENTFERNT: CustomScript { predicate: String }
}
impl WaitCondition { pub fn validate(&self, limits: &BrowserLimits) -> Result<()>; }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(try_from = "u64", into = "u64")]
pub struct WaitTimeout(u64);   // WAR: WaitTimeout(Duration), Serde {"secs","nanos"} → JETZT Integer-Millis 1..=120_000
impl WaitTimeout {
    pub fn from_millis(millis: u64) -> Self;          // klemmt auf ≤ HARD_MAX_WAIT_MS (0 bleibt 0 → validate/Serde lehnen ab)
    pub fn try_from_millis(millis: u64) -> Result<Self>;
    pub fn millis(&self) -> u64;
    pub fn duration(&self) -> std::time::Duration;    // unverändert
    pub fn validate(&self, limits: &BrowserLimits) -> Result<()>;
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct WaitOutcome { /* unverändert */ }
```

### `harw_tool_browser::types`

```rust
#[serde(deny_unknown_fields)] pub struct BrowserToolDescriptor { pub name, pub capability, pub input_schema }  // + Attribut

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
pub struct OpenRequest {                      // WAR: { pub request: OpenBrowserRequest }
    pub start_url: url::Url,
    pub headless: bool,
    pub bidi: BiDiRequirement,
    pub viewport: Option<Viewport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserOpenGrant { /* private: allowed_origins, authentication_origins, profile, limits (NEU) */ }
impl BrowserOpenGrant {
    pub fn ephemeral(OriginPolicy, OriginPolicy) -> Self;                          // unverändert
    pub fn persistent(OriginPolicy, OriginPolicy, impl Into<String>) -> Self;      // unverändert
    #[must_use] pub fn with_limits(self, limits: BrowserLimits) -> Self;           // NEU
    pub fn allowed_origins(&self) -> &OriginPolicy; pub fn authentication_origins(&self) -> &OriginPolicy;
    pub fn profile(&self) -> &ProfilePolicy; pub fn limits(&self) -> &BrowserLimits;   // limits NEU
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserOpenPolicy { /* private grant */ }
impl BrowserOpenPolicy {
    pub fn grant(BrowserOpenGrant) -> Self; pub fn configured_grant(&self) -> Option<&BrowserOpenGrant>;
    pub(crate) fn authorize(&self, model_request: &OpenRequest) -> harw_browser::Result<OpenBrowserRequest>;
    // WAR: authorize(&OpenBrowserRequest); Auth-Origins wurden in allow gemischt (F-114) → jetzt getrennt,
    // Veto auf beide verschärft wenn eines es setzt, Ergebnis via OpenBrowserRequest::validate geprüft.
}

// Alle folgenden: #[serde(deny_unknown_fields)], Felder unverändert
pub struct ObserveRequest; pub struct FindRequest; pub struct ActRequest; pub struct WaitRequest;
pub struct EventsRequest; pub struct CloseRequest;
pub enum BrowserToolRequest { Open(OpenRequest), Observe(..), Find(..), Act(..), Wait(..), Events(..), Close(..) }
pub struct OpenResponse; pub struct ObserveResponse; pub struct FindResponse; pub struct ActResponse;
pub struct WaitResponse; pub struct EventsResponse; pub struct CloseResponse; pub enum BrowserToolResponse;

pub enum BrowserScopeAction { .. }                                  // unverändert

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]                   // WAR: + Deserialize
pub struct BrowserResourceScope { pub session_id, pub context_id, pub origins: Vec<String>, pub action, pub expected_revision }

#[derive(Debug, Clone, PartialEq, Eq)]                              // NEU, kein Serde
pub enum PreparedBrowserRequest {
    Open(OpenBrowserRequest), Observe(ObserveRequest), Find(FindRequest), Act(ActRequest),
    Wait(WaitRequest), Events(EventsRequest), Close(CloseRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]                              // WAR: + Serialize, Deserialize (F-010)
pub struct PreparedBrowserCall { /* private scope, request: PreparedBrowserRequest */ }   // WAR: pub scope, pub(crate) request: BrowserToolRequest
impl PreparedBrowserCall {
    pub(crate) fn new(request: PreparedBrowserRequest, scope: BrowserResourceScope) -> Self;
    pub fn scope(&self) -> &BrowserResourceScope;                    // NEU (Feld privat)
    pub fn request(&self) -> &PreparedBrowserRequest;               // WAR: -> &BrowserToolRequest
    pub(crate) fn into_request(self) -> PreparedBrowserRequest;
}
```

Keine untagged-Enums, kein `#[serde(other)]`, kein `flatten` in den besessenen Dateien.

## Tests (neu/angepasst, alle in den besessenen Dateien)

- **Origin-Wechsel abgelehnt**: `policy::test_is_allowed_{scheme,port,host}_change_denied`,
  `types::test_authorize_rejects_origin_change_in_start_url`, `policy::test_open_browser_request_navigation_vs_observed_location`.
- **Subdomain-Regeln**: `policy::test_is_allowed_exact_rule_does_not_admit_subdomains`,
  `policy::test_is_allowed_wildcard_rule_admits_strict_subdomains_only`, `policy::test_origin_rule_parse_rejects_malformed_rules` (`*.com`, `a.*.b`, `*.IP`).
- **Private-Veto**: `policy::test_is_allowed_private_hosts_vetoed_even_when_listed` (13 Fälle inkl. 169.254.169.254, CGNAT, ULA, NAT64, `localhost.`).
- **Upload/Script abgelehnt**: `action::test_browser_action_deserialize_rejects_{upload,script}_variant(s)`,
  `wait::test_wait_condition_deserialize_rejects_custom_script`, `types::test_act_request_rejects_upload_and_script_actions`,
  `types::test_wait_request_rejects_custom_script_and_bad_timeout`.
- **Unbekannte Felder**: `action::test_browser_action_deserialize_rejects_unknown_fields`, `action::test_action_request_deserialize_rejects_unknown_fields`,
  `wait::test_wait_condition_deserialize_rejects_unknown_fields`, `policy::test_origin_policy_serde_round_trip_and_strictness`,
  `policy::test_open_browser_request_serde_round_trip_and_unknown_field_rejected`, `types::test_request_types_reject_unknown_fields`,
  `types::test_open_request_rejects_model_supplied_authority_fields`.
- **Limits**: `policy::test_browser_limits_*` (Default ≤ Hard, Klemmen, Serde-Bereich), `action::test_validate_*` (Text, Selektor, Fallbacks, Key, Scroll, Navigate),
  `action::test_action_budget_try_consume_until_exhausted`, `wait::test_wait_timeout_*`, `wait::test_wait_condition_validate_limits`.
- **F-114**: `types::test_authorize_keeps_authentication_origins_separate`, `types::test_authorize_rejects_authentication_origin_as_start`,
  `policy::test_open_browser_request_validate_rejects_authentication_origin_as_start`.

## Verifikation durch Lesen

- Imports lösen auf: `crate::error::{Error, Result}` (`error.rs:61`, Varianten `OriginNotAllowed{origin}`,
  `InvalidArgument{detail}` `error.rs:88-99`), `From<url::ParseError>` (`error.rs:162`, für `?` in Doc-Beispielen),
  `crate::selector::{Selector, Target}` (Varianten exhaustiv gematcht, `Target::candidates`, `.fallbacks`),
  `crate::ids::*::new/initial/next`, `harw_browser::observation::ObservationMode`.
- `url` 2.5.8: `Url::host() -> Option<Host<&str>>`, `port_or_known_default`, `username`, `password`, `origin().ascii_serialization()`.
- Stabilität MSRV 1.85: `Ipv4Addr::{is_link_local,is_documentation,is_broadcast}`, `Ipv6Addr::to_ipv4_mapped`, `u32::unsigned_abs`,
  `usize::from(bool)`, `[char; N]` als `Pattern` — alle stabil ≤ 1.85; `Ipv6Addr::is_unique_local` bewusst manuell berechnet.
- Private Repr-Typen (`OriginPolicyRepr`, `BrowserLimitsRepr`) nur in `impl TryFrom<Repr>` (Impl-Sichtbarkeit privat, kein Lint).
- Keine `unwrap`/`expect` außerhalb `#[cfg(test)]`; kein `unsafe`; keine neuen `#[allow]`.

## Folgearbeit (NICHT geändert – bricht bis zur Anpassung den Build)

### W5 B-TOOL (`harw-tool-browser/src/{prepare,dispatch,harness_provider,tool_set,lib}.rs`, Tests)

- `prepare.rs:55-65`: `authorize(&open.request)` → `authorize(&open)`; Ergebnis als `PreparedBrowserRequest::Open(authorized)`;
  Scope-Origins aus `authorized.allowed_origins.origins()` (statt `.allow.to_vec()`); alle anderen Arme auf
  `PreparedBrowserRequest::*` abbilden; `PreparedBrowserCall::new(PreparedBrowserRequest, scope)`.
- `prepare.rs:160`: Arm `BrowserAction::Upload { .. }` entfernen.
- `prepare.rs` (Act/Wait/Find/Observe): `ActionRequest::validate`, `WaitCondition::validate`, `WaitTimeout::validate`,
  `validate_target`/`validate_selector` (Observe `DomSelection`) mit Session-Limits (bzw. `BrowserLimits::default()`
  wenn keine Session-Zuordnung) aufrufen; `ActionBudget` je Session führen; `Navigate` gegen die gespeicherte
  `OpenBrowserRequest::check_navigation_target` prüfen (Session-Ownership).
- `dispatch.rs:14-21`: `prepared.scope.*` → `prepared.scope().*`; `match prepared.into_request()` auf
  `PreparedBrowserRequest`; `Open(request) => self.host().open(request)`.
- `harness_provider.rs:119-126`: Beschreibung von `browser.open` (kein `request`-Wrapper mehr, Felder `start_url, headless,
  bidi, viewport`); `:268`: `from_value::<OpenRequest>` bleibt, Schema muss flach werden; `:324-340` Test-`open_request()` und
  `OriginPolicy::new(vec![..])` (`:332`); `:389` `WaitTimeout::from_millis(1)` ok; `:504` Testfall anpassen.
- `lib.rs`: `PreparedBrowserRequest` re-exportieren (derzeit nur über `PreparedBrowserCall::request()` erreichbar, unbenennbar).
- `tests/tool_contract.rs:44-76,233-323`, `tests/tool_dispatch_contract.rs:56-58`: `OriginPolicy::from_origins(["https://…"], ..)`,
  `OpenRequest` flach, `prepared.scope()`, `PreparedBrowserRequest::Open(open)` mit `open.allowed_origins`/`authentication_origins` getrennt;
  Test `host_open_policy_replaces_model_supplied_origins_and_profile` entfällt (Felder nicht mehr darstellbar) → durch Serde-Ablehnung ersetzen.
- `tests/harness_provider_contract.rs:6`: nur Import, prüfen.
- F-115 (`harness_provider.rs:239`, zusätzlich `network_scope().allows(host)`) liegt vollständig bei B-TOOL; C-BROWSER liefert
  dafür `OriginPolicy::rules()`/`origins()`.

### W5 B-ADAPT (`harw-browser-thirtyfour/**`)

- `src/element_actions.rs:61,98`, `src/runtime_impl.rs:55`: `BrowserAction::Upload` entfernen.
- `src/waits.rs:30-33`, `src/wait_script.rs` (ganzes Modul, `:14-31`): `WaitCondition::CustomScript` entfernen, Modul löschen/entleeren.
- `src/navigation.rs:15-21`: `origin_policy().is_allowed(url)` → gespeicherten `OpenBrowserRequest` halten und
  `check_navigation_target(url)` vor, `check_observed_location(current_url)` **nach jeder** Aktion/Navigation/Wait (F-009).
- `src/runtime.rs:10,39,58,95`: `OriginPolicy`-Feld → `OpenBrowserRequest` (bzw. beide Policies + Limits).
- `src/host.rs:100-105`: `validate_open_request` → `request.validate()`; Limits an Runtime weiterreichen (`ActionBudget`, `max_wait_ms`).
- `src/wait_elements.rs`, `src/wait_navigation.rs`, `src/waits.rs`: `timeout.duration()` bleibt gültig; Wartezeit zusätzlich mit `limits.max_wait_ms()` deckeln.
- `tests/{adapter_contract.rs:19-25,bidi_api_contract.rs:19,profile_contract.rs:17,page_bridge_live_contract.rs:41}`:
  `OpenBrowserRequest` um `authentication_origins`, `limits` ergänzen; `OriginPolicy::from_origins(["https://…"], true)?`.

### Orchestrator / Eigentümer außerhalb W3/W5-Tabellen

- `harw-browser/src/lib.rs:52-62` (Crate-Doku, `no_run`-Doctest kompiliert): `OriginPolicy::new(vec!["example.com".to_owned()], true)`
  → `OriginPolicy::from_origins(["https://example.com"], true)?`, zusätzlich `authentication_origins: OriginPolicy::default()`,
  `limits: BrowserLimits::default()`.
- `harw-browser/tests/session_lifecycle.rs:173-195`: gleiche Anpassung; `is_allowed` bleibt.
- `harw-browser/src/error.rs`: optional neue Variante `LimitExceeded { limit: &'static str, value: u64, max: u64 }`; bis dahin
  melden Limits `InvalidArgument` (Budget-Erschöpfung ebenfalls). Doku `error.rs:31-33` erwähnt nur Typen, bleibt gültig.
- `harw-browser/src/selector.rs`: `Selector`/`Target` ohne `deny_unknown_fields` → unbekannte Felder in `target`/`TagClass`/`Role`
  werden noch still ignoriert (einziger verbleibender Serde-Lax-Punkt im Act/Wait/Find-Pfad). Empfehlung: Attribut ergänzen.
- `harw-browser/src/observation.rs`: `ObservationMode` ohne `deny_unknown_fields` (`DomSelection { selector }`); dito.
- `harw-registry-defaults/src/profile.rs:59,719`: unverändert kompatibel (`BrowserToolSet::new`).
- `harw-channel-browser`: im Workspace nicht mehr vorhanden (X0) – keine Folgearbeit.

## Offene Annahmen

1. Bare-Host-Regeln (`"example.com"`) sind bewusst **nicht** mehr zulässig (Scheme Pflicht); Konfiguration (C-CFG `browser_toml`)
   muss Origins im Format `https://host[:port]` bzw. `https://*.domain` liefern.
2. `about:blank`/`data:` sind nie „erlaubte Location“; B-ADAPT darf den initialen `about:blank` vor der ersten Navigation
   ausnehmen, muss danach aber `check_observed_location` anwenden.
3. Limit-Defaults (200 Aktionen, 30 s Wait, 4 KiB Text, 512 B Selektor, 4 Fallbacks, 2 KiB URL) und Hard-Ceilings sind
   Vorschläge; Anpassung nur über `BrowserOpenGrant::with_limits` (Host-Seite).
