# W3 — C-EGRESS: `harw-egress` (Adressklassen, Policy, Scoped-Resolver, Client)

Owned: `harw-egress/**` (neu), dieses Ledger. BUILD-POLICY eingehalten: nichts gebaut, geprüft, getestet, formatiert;
keine git-Schreibbefehle. Verifikation durch Lesen (Signaturen gegen `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/…`).
Befunde: F-168 (keine Sperre privater/Metadaten-Ziele, Wildcard-DNS hinter `DnsSuffix`), F-035/F-062 (SSRF über Redirect-
bzw. Cache-Pfad; hier nur die Grundlage: kein Redirect-Folgen, Prüfung je Hop — Cache ist N-WEB).
Workspace-Mitgliedschaft `"harw-egress"` trägt **X0** in Root-`Cargo.toml` ein.

## 1. Dateien

| Datei | Inhalt |
|---|---|
| `harw-egress/Cargo.toml` | `version/edition/rust-version.workspace = true`, `[lints] workspace = true` |
| `src/lib.rs` | Modulkopf, Re-Exporte |
| `src/classify.rs` | `AddrClass`, `classify` + Tabellen-Tests |
| `src/policy.rs` | `EgressPolicy` + Tests (URL, Userinfo, IP-Literal, Suffix, Digest) |
| `src/client.rs` | `build_client`, `ScopedResolver`, `filter_resolved`, `HostLookup` + Tests (Stub-Resolver, Loopback-Server) |
| `src/error.rs` | `EgressError` (handgeschrieben: `Display`, `Error::source`, `From<EgressUrlError>`, `From<reqwest::Error>`) |

## 2. Öffentliche API (wie geschrieben)

```rust
// lib.rs
pub use classify::{AddrClass, classify};
pub use client::build_client;
pub use error::EgressError;
pub use harw_sandbox::egress::{EgressHost, EgressUrl, EgressUrlError};   // wiederverwendet, nicht dupliziert
pub use policy::EgressPolicy;

// classify.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddrClass { Loopback, Private, LinkLocal, CloudMetadata, Cgnat, UniqueLocal, Multicast,
                     Documentation, Benchmark, Reserved, Unspecified, Broadcast, Public }
impl AddrClass { pub const fn is_public(self) -> bool; }
impl Display for AddrClass;                // "loopback", "cloud-metadata", "unique-local", …
pub fn classify(addr: IpAddr) -> AddrClass;

// policy.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EgressPolicy { /* allow_hosts: Vec<String> (kanonisch), allow_private: bool */ }
impl EgressPolicy {
    pub fn new(allow_hosts: Vec<String>, allow_private: bool) -> Result<Self, EgressError>;
    pub fn allow_hosts(&self) -> &[String];                      // zusätzlich (lesend)
    pub fn allow_private(&self) -> bool;                         // zusätzlich (lesend)
    pub fn check_url(&self, url: &str) -> Result<EgressUrl, EgressError>;
    pub fn check_addr(&self, addr: SocketAddr) -> Result<(), EgressError>;
    pub fn digest(&self) -> [u8; 32];
    pub(crate) fn check_host(&self, host: &str) -> Result<(), EgressError>;
}

// client.rs
pub fn build_client(policy: Arc<EgressPolicy>) -> Result<reqwest::Client, EgressError>;
pub(crate) type LookupFuture = Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>>;
pub(crate) trait HostLookup: Send + Sync { fn lookup(&self, host: &str) -> LookupFuture; }
pub(crate) fn build_client_with_lookup(policy: Arc<EgressPolicy>, lookup: Arc<dyn HostLookup>)
    -> Result<reqwest::Client, EgressError>;
pub(crate) fn filter_resolved<I: IntoIterator<Item = SocketAddr>>(policy: &EgressPolicy, host: &str, resolved: I)
    -> Result<Vec<SocketAddr>, EgressError>;

// error.rs
#[derive(Debug)]
pub enum EgressError {
    InvalidUrl(EgressUrlError),
    InvalidAllowEntry { entry: String, reason: &'static str },
    HostNotAllowed { host: String },
    LocalHostName { host: String },
    AddressDenied { addr: IpAddr, class: AddrClass },
    NoPermittedAddress { host: String, denied: Vec<(IpAddr, AddrClass)> },
    Lookup { host: String, source: std::io::Error },
    ClientBuild(reqwest::Error),
}
```

`pub(crate)`-Items (`check_host`, `filter_resolved`, `HostLookup`) sind für N-EGRESS (`src/proxy.rs`, W5) gedacht: der
SOCKS5-Proxy nutzt dieselbe Allowlist- und Adressregel.

## 3. Semantik

**Klassifikation** (`classify`, IANA Special-Purpose Registries; erste Regel gewinnt):
- IPv4 exakt: `169.254.169.254`, `169.254.170.2`, `169.254.170.23`, `100.100.100.200` → `CloudMetadata`;
  `255.255.255.255` → `Broadcast`; `0.0.0.0` → `Unspecified`.
- IPv4-Präfixe: `0/8` Reserved · `10/8` Private · `100.64/10` Cgnat · `127/8` Loopback · `169.254/16` LinkLocal ·
  `172.16/12` Private · `192.0.0/24` Reserved · `192.0.2/24` Documentation · `192.88.99/24` Reserved (6to4-Relay) ·
  `192.168/16` Private · `198.18/15` Benchmark · `198.51.100/24`, `203.0.113/24` Documentation · `224/4` Multicast ·
  `240/4` Reserved · sonst Public.
- IPv6 exakt: `::` Unspecified · `::1` Loopback · `fd00:ec2::254`, `fd00:ec2::23` CloudMetadata.
- `::ffff:0:0/96` (v4-mapped) und `64:ff9b::/96` (NAT64 WKP) → Klasse der eingebetteten IPv4.
- IPv6-Präfixe: `::/96` (v4-kompatibel) · `::ffff:0:0:0/96` (SIIT) · `64:ff9b:1::/48` · `100::/64` → Reserved ·
  `2001:2::/48` Benchmark · `2001:db8::/32`, `3fff::/20` Documentation · `2001::/23` (inkl. Teredo) · `2002::/16`
  (6to4) → Reserved (konservativ, ohne Blick auf die eingebettete IPv4) · `fc00::/7` UniqueLocal · `fe80::/10`
  LinkLocal · `fec0::/10` Reserved · `ff00::/8` Multicast · außerhalb `2000::/3` Reserved · sonst Public.

**Policy:**
- Allowlist-Einträge: IPv4/IPv6-Literal (IPv6 auch in `[]`) → `IpAddr::to_string()`; sonst nur `[A-Za-z0-9._-]` plus
  Nicht-ASCII, kein führender Punkt, dann `url::Host::parse` (IDNA, Kleinschreibung, IPv4-Kurzformen), ein abschließender
  Punkt weg, keine leeren Labels. Abgelehnt u. a. `*.docs.rs`, `https://docs.rs`, `docs.rs:443`, `user@docs.rs`,
  `%64ocs.rs`, ` docs.rs`, `a..b`. Danach sortiert + dedupliziert.
- Leere Allowlist → **nichts** erlaubt.
- Adressklassen: `Public` immer; mit `allow_private` zusätzlich `Loopback`, `Private`, `Cgnat`, `UniqueLocal`.
  **Nie**: `CloudMetadata`, `LinkLocal`, `Multicast`, `Documentation`, `Benchmark`, `Reserved`, `Unspecified`,
  `Broadcast` — auch nicht, wenn die IP wörtlich in der Allowlist steht.
- `check_url`: `EgressUrl::parse` (harw-sandbox; WHATWG, http/https, Userinfo verboten) → bei IP-Literal `check_addr`
  → `check_host` (`localhost`/`*.localhost` ohne `allow_private` → `LocalHostName`; dann `host_matches_suffix`).
- `digest`: BLAKE3 über `b"harw:egress-policy:v1\0" ‖ u8(allow_private) ‖ u64le(n) ‖ Σ(u64le(len) ‖ bytes)` der
  kanonischen Einträge. Reihenfolge/Groß-Klein/Duplikate/abschließender Punkt ändern ihn nicht.

**Client (`build_client`):** `reqwest::Client::builder().dns_resolver(Arc<ScopedResolver>).no_proxy()
.redirect(Policy::none()).connect_timeout(10 s).build()`. `ScopedResolver::resolve(name)`: Name kleinschreiben →
`check_host` (vor jedem Lookup) → `tokio::net::lookup_host((host, 0))` → `filter_resolved` (jede Adresse `check_addr`,
verworfene mit `tracing::warn!(host, addr, class)`; keine übrig → `NoPermittedAddress`). hyper-util verbindet nur zu den
zurückgegebenen Adressen → kein Rebinding-Fenster. Fehler reisen als `Box<dyn Error + Send + Sync>` in der
`source()`-Kette des `reqwest::Error` (hyper-util `ConnectError::source` gibt die Ursache zurück).

## 4. API-Belege (Registry-Pfade, Versionen aus Cargo.lock)

Basis: `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`

| Item | Beleg |
|---|---|
| `reqwest::dns::{Resolve, Resolving, Name, Addrs}`; `Resolve: Send + Sync`, `fn resolve(&self, Name) -> Resolving` | `reqwest-0.12.28/src/dns/resolve.rs:15-34`, Re-Export `src/dns/mod.rs:3`, `pub mod dns` `src/lib.rs:376` |
| `Name: FromStr` (Err `InvalidNameError: Debug`), `Name::as_str` | `reqwest-0.12.28/src/dns/resolve.rs:48-63,178-193` |
| Resolver-Port: expliziter URL-Port überschreibt, Port 0 → Schema-Default | `reqwest-0.12.28/src/dns/resolve.rs:31-32`; `hyper-util-0.1.20/src/client/legacy/connect/http.rs:543-551` |
| `ClientBuilder::dns_resolver<R: Resolve + 'static>(Arc<R>)` | `reqwest-0.12.28/src/async_impl/client.rs:2290` |
| `ClientBuilder::no_proxy()` (leert Proxys, `auto_sys_proxy = false`) | `reqwest-0.12.28/src/async_impl/client.rs:1427-1431` |
| `ClientBuilder::redirect(redirect::Policy)`, `redirect::Policy::none()` | `…/client.rs:1383`; `reqwest-0.12.28/src/redirect.rs:58` |
| `ClientBuilder::connect_timeout(Duration)`, `build() -> crate::Result<Client>` | `…/client.rs:1466`, `:411` |
| **IP-Literal umgeht Resolver** (`SocketAddrs::try_parse` vor `resolve`) | `hyper-util-0.1.20/src/client/legacy/connect/http.rs:538-541` |
| `ConnectError::source` → Ursache (Downcast auf `EgressError` möglich) | `hyper-util-0.1.20/src/client/legacy/connect/http.rs:699-703`, `dns()` `:664-669` |
| Feature `rustls-tls` → webpki-roots + ring | `reqwest-0.12.28/Cargo.toml:148,164-167` |
| `tokio::net::lookup_host<T: ToSocketAddrs>` (Feature `net`, `spawn_blocking`) | `tokio-1.53.0/src/net/lookup_host.rs:32`, `src/net/addr.rs:182`, `Cargo.toml:96` |
| `url::Host::<String>::parse` (IDNA, v4-Kurzformen, `[v6]`) | `url-2.5.8/src/host.rs:81,parse_cow` |
| `blake3::Hasher::{new,update,finalize}`, `From<Hash> for [u8; 32]`, `Hash::as_bytes` | `blake3-1.8.7/src/lib.rs:1089,1204,1398,326,247` |
| `EgressUrl::{parse,host,host_str,port,as_url}`, `EgressHost::{Ip,Domain,is_loopback}`, `host_matches_suffix` | `harw-sandbox/src/egress.rs:227,263,273,282,257,81-86,108,327`; Re-Export `harw-sandbox/src/lib.rs:44-45` |

## 5. Abhängigkeiten (alle Versionen bereits in Cargo.lock; kein `cargo add` nötig)

| Dep | Zeile | Lock |
|---|---|---|
| harw-sandbox | `{ path = "../harw-sandbox" }` | Workspace |
| blake3 | `{ workspace = true }` | 1.8.7 |
| tracing | `{ workspace = true }` | 0.1.44 |
| reqwest | `{ version = "0.12.28", default-features = false, features = ["json", "rustls-tls"] }` (identischer Feature-Satz wie harw-tool-web/harw-core; **nicht** 0.13.4, sonst inkompatibler `Client` für N-WEB) | 0.12.28 |
| tokio | `{ version = "1.53.0", features = ["net"] }`; dev: `["rt", "macros", "net"]` | 1.53.0 |
| url | `"2.5.8"` | 2.5.8 |

Kein hickory: nicht in Cargo.lock; System-Resolver über tokio genügt. Keine `dep-request`-Einträge.

## 6. Tests (30, alle netzfrei; einer nutzt einen Loopback-`TcpListener`)

- classify: `test_classify_ipv4_table`, `test_classify_ipv6_table`, `test_classify_ipv4_mapped_uses_inner_address`,
  `test_classify_nat64_uses_inner_address`, `test_addr_class_is_public_and_display`.
- policy: `test_new_normalizes_sorts_and_dedups`, `test_new_rejects_invalid_entries`,
  `test_check_url_backslash_at_targets_evil_host` (`https://evil.com\@docs.rs/` → `HostNotAllowed{evil.com}`),
  `test_check_url_rejects_userinfo`, `test_check_url_rejects_private_ip_literals_even_if_allowlisted` (inkl. `0x7f.1`,
  `2130706433`, `[::ffff:127.0.0.1]`, `[64:ff9b::a9fe:a9fe]`), `test_check_url_private_ip_literal_with_allow_private`,
  `test_check_url_host_suffix_boundaries`, `test_check_url_empty_allowlist_denies_everything`,
  `test_check_url_localhost_requires_allow_private`, `test_check_url_rejects_non_http_scheme`,
  `test_check_addr_class_matrix`, `test_digest_is_stable_under_normalization`, `test_digest_distinguishes_policies`,
  `test_digest_matches_documented_encoding`.
- client: `test_filter_resolved_drops_private_answers`, `test_filter_resolved_all_private_is_error_with_classes`,
  `test_filter_resolved_empty_answer_is_error`, `test_filter_resolved_allow_private_keeps_lan_but_not_metadata`,
  `test_scoped_resolver_filters_private_answer` (Stub-Lookup über `Resolve::resolve`),
  `test_scoped_resolver_rejects_host_outside_allowlist_before_lookup`,
  `test_build_client_request_to_rebound_name_fails_without_connecting` (Name → 127.0.0.1, `NoPermittedAddress` in der
  `source()`-Kette), `test_build_client_connects_only_to_checked_address` (Loopback-Server mit `allow_private`, 302 wird
  **nicht** verfolgt).
- error: `test_display_address_denied_names_address_and_class`, `test_display_no_permitted_address_lists_denied`,
  `test_from_egress_url_error_keeps_source`.

Nicht getestet (Policy: kein `set_var`): dass `no_proxy()` `HTTP_PROXY` ignoriert — belegt nur durch Quelle (§4).

## 7. Annahmen / Entscheidungen (für Review)

1. **Leere Allowlist = kein Ziel** (fail closed, passt zu „Netz leer bis Teil B“). Kein Wildcard-Eintrag.
2. `allow_private` erlaubt Loopback/RFC1918/CGNAT/ULA, **nicht** Link-Local (dort liegen weitere Metadaten-Dienste wie
   ECS `169.254.170.2`) und nie Cloud-Metadaten/Reserved/Doku/Benchmark/Multicast/Broadcast/Unspecified.
3. Metadaten-Liste um `169.254.170.2`, `169.254.170.23`, `fd00:ec2::23` (AWS ECS/EKS) über den Brief hinaus ergänzt.
4. Konservativer als `harw_sandbox::egress::is_private_or_special`: `2001::/23` (außer Benchmark) und IPv6 außerhalb
   `2000::/3` gelten als Reserved (Sandbox-Test hält z. B. `2001:1::1` für öffentlich). NAT64 WKP mit öffentlicher
   eingebetteter IPv4 ist dagegen **Public** (DNS64-Netze), wie im Brief gefordert.
5. `check_url` prüft bei IP-Literalen die Adressklasse **vor** der Allowlist (spezifischere Fehlermeldung).
6. Der Resolver prüft die Allowlist zusätzlich selbst (Defense in Depth, falls ein Aufrufer `check_url` vergisst) —
   IP-Literale erreicht er aber nie (hyper-util, §4). `check_url` vor jedem Senden und je Redirect-Hop bleibt Pflicht
   (N-WEB).
7. `connect_timeout(10 s)` im Client; Gesamt-Deadlines setzen Aufrufer.
8. `EgressError` handgeschrieben statt `HarwError`-Derive (volle Kontrolle über `source()`, keine Proc-Macro-Dep).
9. `rustfmt` nicht ausgeführt (Policy); Zeilen manuell ≤ 100 Zeichen gehalten — `cargo fmt --check` durch Orchestrator.

## 8. Ausgabe

```json
{"agent":"C-EGRESS",
 "files_created":["/home/mia/projects/harwness/harw-egress/Cargo.toml",
  "/home/mia/projects/harwness/harw-egress/src/lib.rs","/home/mia/projects/harwness/harw-egress/src/classify.rs",
  "/home/mia/projects/harwness/harw-egress/src/policy.rs","/home/mia/projects/harwness/harw-egress/src/client.rs",
  "/home/mia/projects/harwness/harw-egress/src/error.rs",
  "/home/mia/projects/harwness/docs/remediation/ledger/W3/C-EGRESS.md"],
 "files_modified":[],
 "verification":{"command":"read-only, parent builds (cargo check/clippy --tests/test -p harw-egress nach X0)","exit_code":null,"pass":null},
 "stubbed_imports":[{"module":"workspace member harw-egress","reason":"X0 trägt Mitgliedschaft in Root-Cargo.toml ein"}],
 "blocked":false}
```
