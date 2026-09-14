# W5 — N-WEB: `harw-tool-web` auf `harw-egress` (SSRF, Cache-Poisoning, Kappung)

Owned: `harw-tool-web/src/**`, `harw-tool-web/Cargo.toml`, dieses Ledger. BUILD-POLICY eingehalten: nichts gebaut,
geprüft, getestet, formatiert; keine git-Schreibbefehle. Verifikation durch Lesen (API-Belege §5).
Befunde: **F-035** (Cache-Treffer ohne Endziel-Prüfung, prozessweit geteilt), **F-062** (SSRF über Cache, `/tmp`-Cache
ohne Eigentümerprüfung), **F-037** (script/style/versteckter Text im Modellkontext), **F-169** (keine Textkappung,
synchrones Parsen im async-Kontext), **F-138** (Cache-Dir ignoriert `HARW_HOME`).

## 1. Dateien

| Datei | Status | Inhalt |
|---|---|---|
| `Cargo.toml` | geändert | + `harw-egress`, `harw-fsutil` (path), `blake3` (workspace); − `sha2` (tot); tokio `["rt","time"]` (− `sync`, `macros` nur dev); reqwest `0.12.28` wie harw-egress; scraper/htmd auf Lock-Versionen (TODOs entfernt) |
| `src/hop.rs` | **neu** | `HopTarget`, `check_hop`, `resolve_location`, `map_send_error` (+ adressfreie Fehlerübersetzung) |
| `src/cache.rs` | **neu** | `CacheScope`, `cache_key`, `cache_path`, `CacheEntry` v2, `entry_matches`, `read_cache_entry`/`write_cache_entry` über harw-fsutil |
| `src/html.rs` | **neu** | `REMOVED_ELEMENTS`, `sanitize_html`, `html_to_text`, `html_to_markdown`, `truncate_utf8` |
| `src/fetch.rs` | neu geschrieben | `WebFetcher` (Policy-Parameter, Egress-Client), `WebFetchOptions`, `fetch`/`fetch_source`/`validate_cached`, `run_blocking`, `finish_document`, Tool `web.fetch` |
| `src/error.rs` | geändert | + `EgressDenied{reason}`, `NotConfigured{what}`, `BlockingTask{task}` |
| `src/provider.rs` | geändert | `configure(policy, cache_dir, options)` |
| `src/docs_rs.rs`, `src/crates_io.rs` | geändert | `fetch_source` + Parsen/Konvertieren in `run_blocking` |
| `src/lib.rs` | geändert | Module `cache`/`hop`/`html`, Re-Exporte, Sicherheitskontrakt-Doku |

## 2. Öffentliche API (wie geschrieben; Brüche gegenüber vorher markiert)

```rust
// provider.rs  (BRUCH: vorher configure(cache_dir, ttl, max_bytes))
pub fn configure(policy: Arc<EgressPolicy>, cache_dir: PathBuf, options: WebFetchOptions) -> WebToolResult<()>;

// fetch.rs
pub const MAX_REDIRECTS: usize = 5;              // vorher 3
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;  pub const HARD_MAX_OUTPUT_BYTES: usize = 1_048_576;
pub struct WebFetchOptions { pub ttl: Duration, pub max_bytes: usize, pub max_output_bytes: usize, pub allow_http: bool }
impl Default for WebFetchOptions;                // nur Limits, keine Ziel-Freigaben; allow_http = false
pub struct FetchedDocument { url, status, content_type, body, from_cache, etag, pub truncated: bool }  // + truncated
impl WebFetcher {
    pub fn new(policy: Arc<EgressPolicy>, cache_dir: PathBuf, options: WebFetchOptions) -> WebToolResult<Self>; // BRUCH
    pub fn scoped(&self, network: NetworkScope, cache_scope: CacheScope, max_bytes: Option<usize>) -> Self;    // BRUCH
    pub fn cache_dir(&self) -> &Path; pub fn max_bytes(&self) -> usize; pub fn options(&self) -> WebFetchOptions;
    pub fn policy(&self) -> &EgressPolicy; pub fn cache_scope(&self) -> CacheScope;
    pub fn check_target(&self, url: &str, hop: usize) -> WebToolResult<HopTarget>;
    pub fn validate_cached(&self, entry: &CacheEntry, key_hex: &str, request_url: &str) -> WebToolResult<()>;
    pub async fn fetch(&self, url: &str, format: OutputFormat) -> WebToolResult<FetchedDocument>;
    pub async fn fetch_source(&self, url: &str) -> WebToolResult<FetchedDocument>;   // Rohkörper, nur byte-gekappt
}
pub fn shared_fetcher() -> WebToolResult<Arc<WebFetcher>>;   // BRUCH: kein Lazy-Default mehr → NotConfigured
pub fn install_fetcher(fetcher: Arc<WebFetcher>) -> Result<(), Arc<WebFetcher>>;
pub fn scoped_fetcher(context: &ToolExecutionContext, max_bytes: Option<usize>) -> WebToolResult<WebFetcher>;
pub fn check_content_type(raw: &str, host: &str) -> WebToolResult<String>;
pub fn push_chunk(buffer: &mut Vec<u8>, chunk: &[u8], limit: usize, host: &str) -> WebToolResult<()>;
pub fn render(raw: &str, content_type: &str, format: OutputFormat) -> WebToolResult<String>;
pub fn finish_document(document: FetchedDocument, format: OutputFormat, max_output_bytes: usize) -> WebToolResult<FetchedDocument>;
pub async fn run_blocking<T: Send + 'static, F: FnOnce() -> WebToolResult<T> + Send + 'static>(task: &'static str, job: F) -> WebToolResult<T>;
pub fn document_output(document: &FetchedDocument) -> ToolOutput;   // + "truncated"
// ENTFERNT: validate_target, WebFetcher::with_defaults, default_cache_dir (HARW_CACHE_DIR/$HOME/tmp), fetch::html_to_*,
//           fetch::cache_path(base,url)/read/write/entry_is_fresh (→ cache.rs), fetch::CacheEntry (→ cache::CacheEntry)

// hop.rs
pub struct HopTarget; impl HopTarget { pub fn url(&self) -> &str; pub fn host(&self) -> &str; }
pub fn check_hop(policy: &EgressPolicy, scope: &NetworkScope, allow_http: bool, url: &str, hop: usize) -> WebToolResult<HopTarget>;
pub fn resolve_location(current: &str, location: &str, hop: usize) -> WebToolResult<String>;
pub fn map_send_error(error: reqwest::Error, hop: usize) -> WebToolError;

// cache.rs
pub const CACHE_SUBDIR: &str = "web"; pub const CACHE_FORMAT_VERSION: u32 = 2; pub const MAX_CACHE_FILE_BYTES: u64 = 64 MiB;
pub struct CacheScope; impl CacheScope { new(tenant,&str workspace,&NetworkScope); from_context(&ToolExecutionContext); unbound(); digest() }
pub fn cache_key(policy_digest: &[u8; 32], scope: &CacheScope, url: &str) -> [u8; 32];
pub fn cache_path(base: &Path, key: &[u8; 32]) -> PathBuf;  pub fn to_hex(bytes: &[u8]) -> String;
pub struct CacheEntry { format_version, key, chain: Vec<String>, status, etag, fetched_at, content_type, body }  // deny_unknown_fields
pub fn entry_matches(..) -> bool; pub fn entry_is_fresh(..) -> bool; pub fn now_secs() -> u64;
pub fn read_cache_entry(path: &Path) -> WebToolResult<Option<CacheEntry>>;
pub fn write_cache_entry(base: &Path, path: &Path, entry: &CacheEntry) -> WebToolResult<()>;

// html.rs
pub const REMOVED_ELEMENTS: &[&str] = &["script", "style", "noscript", "template"];
pub fn sanitize_html(&str) -> String; pub fn html_to_text(&str) -> String;
pub fn html_to_markdown(&str) -> WebToolResult<String>; pub fn truncate_utf8(&str, usize) -> (&str, bool);
```

## 3. Semantik

**Client:** ausschließlich `harw_egress::build_client(Arc::clone(&policy))` (Scoped-Resolver, `no_proxy`,
`redirect::Policy::none`, connect 10 s). Pro Anfrage `User-Agent` + `timeout(20 s)`, Gesamt-Deadline 45 s.

**Policy-Quelle (verifiziert):** heute gibt es keine Verdrahtung — `WebToolProvider` ist eine Unit-Struktur
(`tool_provider!`), die Allowlist kam nur aus `context.sandbox().network_scope()`. Neu: `Arc<EgressPolicy>` als
Konstruktor-/`configure`-Parameter, **ohne Default**; ohne `configure` liefert jeder Tool-Aufruf
`NotConfigured`. Vorgesehene Quelle: `harw_config::NetworkSection{allow_hosts, allow_private}` (C-CFG) →
`EgressPolicy::new`, Cache-Dir `harw_home::paths::cache_dir(home)` (= `<HARW_HOME>/cache`). Pro Aufruf zusätzlich der
Sandbox-`NetworkScope` (Macro-Prolog `host_from="url"` für die Start-URL, `check_hop` für jeden Hop): **beide** Grenzen
müssen bestehen.

**Hop-Prüfung (`check_hop`)**, vor jedem Senden, für Start-URL, jedes Redirect-Ziel und jede URL eines Cache-Eintrags:
`EgressUrl::parse` → Schema `https` (`http` nur `allow_http`) → `EgressPolicy::check_url` (Allowlist, `localhost`,
Adressklasse von IP-Literalen, die den Resolver umgehen) → `NetworkScope::allows`. Redirects manuell, max. 5 Hops;
`304` nur als Antwort auf den eigenen Conditional-GET an Hop 0.

**Fehler ohne interne Adressen:** `EgressError` → `EgressDenied{reason}` mit Host (IP-Literale → `<IP-Literal>`) bzw.
Adressklasse (`loopback`, `cloud-metadata` …), nie IP. URLs aus `Location` erscheinen nur als `<Weiterleitung n>`.
`reqwest::Error` → `without_url()`. Resolver-Ablehnungen in der `source()`-Kette (`downcast_ref::<EgressError>`) werden
`EgressDenied` (kein Transportfehler → kein Cache-Fail-open); nur `EgressError::Lookup` bleibt Transport.

**Cache:** Schlüssel `blake3("harw:web-cache-key:v2\0" ‖ policy.digest() ‖ scope ‖ lp(url))`, `scope =
blake3("harw:web-cache-scope:v1\0" ‖ lp(tenant) ‖ lp(workspace) ‖ u64le(n) ‖ Σ lp(target-wire))`. `url` ist die
normalisierte **Anfrage**-URL (vor dem Senden bekannt); das Endziel steht in `chain` und wird bei jedem Treffer geprüft.
Treffer (frisch, 304-Refresh **und** Stale-Fallback) nur nach `validate_cached`: Version/Schlüssel/`chain[0]` gleich,
`chain.len() ≤ 6`, jede Kettenposition besteht `check_target` in unveränderter Normalform, Content-Type noch erlaubt,
Körper ≤ aktuelles `max_bytes`. Sonst `warn!("web.cache.discarded")` und Miss. Dateien: `<cache_dir>/web/<hex>.json`,
Verzeichnis `0700` (zu offene Rechte → `chmod 0700`, Symlink/Nicht-Dir → Fehler), Schreiben `write_atomic` `0600`,
Lesen `open_nofollow` + `ensure_private_regular` (eigener UID, keine g/o-Rechte) + 64-MiB-Leseobergrenze.
Kein `/tmp`-, `HARW_CACHE_DIR`- oder `$HOME`-Fallback mehr (F-138/F-062). Alte v1-Einträge → `CacheCorrupt` → Miss.

**Kappung:** Bytes beim Lesen (`Content-Length`-Vorprüfung + `push_chunk`, vor `from_utf8_lossy`); Text nach
Aufbereitung `truncate_utf8(max_output_bytes)` (Default 64 KiB, hart 1 MiB), `truncated` in Ausgabe. Modell-`max_bytes`
kann das konfigurierte Limit nur senken (vorher bis `HARD_MAX_BYTES` anhebbar). docs.rs behält `MAX_DOC_CHARS`.

**HTML:** `sanitize_html` trennt Teilbäume ab (`Html.tree.get_mut(id).detach()`): `script|style|noscript|template`,
`[hidden]`, `aria-hidden="true"`, Inline-Stil `display:none`/`visibility:hidden`. Text- und Markdown-Pfad arbeiten nur
darauf; htmd zusätzlich `skip_tags(REMOVED_ELEMENTS)`.

**Blocking:** Rendern, Markdown, docs.rs-Extraktion, crates.io-JSON, Cache-Lesen/-Schreiben über
`run_blocking` (`tokio::task::spawn_blocking`, `JoinError` → `BlockingTask`).

## 4. Tests (alle netzfrei; Loopback-`TcpListener` in std-Thread mit 10-s-Accept-Deadline)

- hop (10): `test_check_hop_accepts_allowed_https_url`, `…_rejects_private_ip_literals_without_leaking_address`
  (127.0.0.1, 10.0.0.5, `[::ffff:127.0.0.1]`, `2130706433`, alle in Allowlist), `…_rejects_metadata_even_with_allow_private`,
  `…_rejects_host_outside_policy`, `…_rejects_host_outside_sandbox_scope`, `…_http_requires_explicit_opt_in`,
  `…_rejects_userinfo_and_backslash_confusion`, `test_resolve_location_then_check_hop_rejects_private_redirect`
  (Hop-Logik), `test_resolve_location_invalid_base_uses_placeholder`, `test_redact_host_hides_ip_literals_only`.
- cache (12): `test_cache_key_changes_with_policy_digest`, `…_changes_with_scope_and_url`,
  `test_cache_path_is_hex_json_below_web_subdir`, `test_write_then_read_cache_entry_round_trips_with_private_modes`,
  `test_read_cache_entry_missing_file_is_none`, `…_rejects_non_private_file` (F-062), `…_rejects_symlink`,
  `…_reports_corrupt_and_legacy_content`, `test_write_cache_entry_rejects_path_outside_cache_dir`,
  `…_tightens_cache_dir_mode`, `test_entry_matches_requires_version_key_and_request_url`,
  `test_entry_is_fresh_respects_ttl_and_future_timestamps`.
- html (6): `test_html_to_text_removes_script_style_noscript`, `…_removes_hidden_elements`,
  `test_html_to_markdown_removes_script_and_style`, `test_sanitize_html_keeps_visible_markup`,
  `test_truncate_utf8_cuts_before_multibyte_char`, `test_collapse_whitespace_keeps_paragraph_breaks`.
- fetch (20): Content-Type/Chunk/Render/Format/`finish_document` (Mehrbyte-Kappung), `test_shared_fetcher_without_configuration_is_not_configured`,
  `test_scoped_caps_requested_max_bytes`, `test_fetch_without_scope_denies_every_host`,
  `test_fetch_rejects_ip_literal_url_before_network`, `test_fetch_rejects_http_without_opt_in`,
  Loopback: `test_fetch_redirect_to_metadata_ip_is_rejected` (1 Anfrage, keine Adresse in Meldung),
  `…_redirect_to_host_outside_policy_is_rejected`, `…_redirect_chain_beyond_limit_is_rejected` (6 Anfragen),
  `…_follows_allowed_redirect_and_caps_text` (script entfernt, `"ää"` bei 5 Bytes, 2. Abruf aus Cache),
  `…_discards_cache_hit_with_forbidden_final_target`, `…_stale_fallback_never_uses_forbidden_entry`,
  `…_serves_valid_fresh_cache_entry_without_network`, `test_validate_cached_rejects_entry_from_other_policy_key`,
  `…_conditional_get_refreshes_cache_on_304` (prüft `If-None-Match`), Tool-Deklaration/Schema.
- error: + `test_is_transport_egress_denied_is_not_transport`. docs_rs/crates_io/provider: unverändert.
- Entfernt: drei `#[ignore]`-Stubs mit `unimplemented!` (jetzt echte Loopback-Tests).

Hinweis Tests: `test_shared_fetcher_without_configuration_is_not_configured` setzt voraus, dass kein Test des Crates
`install_fetcher` aufruft (Prozess-`OnceLock`). Die „geschlossener Port“-Tests binden und droppen einen Listener;
theoretisches Rennen mit Port-Wiederverwendung.

## 5. API-Belege

Basis `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`
| Item | Beleg |
|---|---|
| `htmd::HtmlToMarkdown::builder().skip_tags(Vec<&str>).build().convert(&str) -> io::Result<String>` | `htmd-0.2.2/src/lib.rs:98,103,175,212` |
| `scraper::Html{pub tree}`, `parse_document`, `html()`, `root_element`, `select` | `scraper-0.23.1/src/html/mod.rs:37,79,97,106,117` |
| `scraper::node::Element::{name, attr}`, `Node::as_element`, `Text: Deref<str>` | `scraper-0.23.1/src/node.rs:95,196,255,299`; `pub mod node` `src/lib.rs:151` |
| `ego_tree::Tree::{root,get_mut}`, `NodeRef::{id,descendants}`, `NodeMut::detach` | `ego-tree-0.10.0/src/lib.rs:190,222,295,511`; `iter.rs:353` |
| `reqwest::Error::without_url`, `RequestBuilder::{header,timeout}`, `Client::get<IntoUrl>` (`&str`) | `reqwest-0.12.28/src/error.rs:88`; `async_impl/request.rs:194,291`; `async_impl/client.rs:2498`; `into_url.rs:11` |
| `harw_egress::{build_client, EgressPolicy::{check_url,digest,allow_hosts}, EgressError, EgressUrl, EgressUrlError}` | `harw-egress/src/lib.rs`, `policy.rs:91-221`, `error.rs:27-72`, `client.rs:110` |
| `EgressUrl::{parse,as_url,host_str,is_https}` | `harw-sandbox/src/egress.rs:257,273,288` |
| `harw_fsutil::{write_atomic, AtomicWriteOptions, open_nofollow, OpenMode::read_only, ensure_private_regular}` | `harw-fsutil/src/atomic.rs:62,150`; `open.rs:61,254`; `perm.rs:30` |
| `SandboxSpec::{workspace,network_scope}`, `WorkspaceBinding::{tenant,workspace}`, `NetworkScope::{targets,allows,from_hosts,empty}`, `EgressTarget::{Host,DnsSuffix,Cidr}` | `harw-sandbox/src/lib.rs` (Stand nach N-SBX-Edits: 235, 330, 355, 491, 545, 728ff, 905ff) |
| `TenantId/WorkspaceId::as_str` | `harw-types/src/ids.rs` (`newtype_id!`) |
| `ToolExecutionContext::sandbox` | `harw-tools/src/executor.rs:58` |
| Workspace-Mitglieder `harw-egress`, `harw-fsutil`; `blake3` in `[workspace.dependencies]` | Root-`Cargo.toml:103-104,129` |

## 6. Aufrufer der geänderten API (Folgearbeit, nicht geändert)

`grep harw_tool_web|harw-tool-web` (rs/toml): **kein Rust-Aufrufer** außerhalb des Crates; nur Root-`Cargo.toml`
(Mitglied) und `README.md:549,865,924` (Doku). Tool-Namen `web.fetch/docs_rs/crates_io` werden referenziert in
`harw-registry-defaults/{src/lib.rs,src/profile.rs,agents/*.toml,agents/context-programs/*.toml,agents/roles/**}`,
`harw-agent-dsl/src/context_program.rs`, `harw-context/src/ceiling.rs`, `harw-runtime/src/ceiling.rs`,
`harw-core/src/{envelope,session,mode}.rs`, `harw-core/tests/session_mode_intersection.rs` — Namen unverändert.
- **RD / W6 I-CONTRIB (Pflicht):** beim Start `harw_tool_web::configure(Arc::new(EgressPolicy::new(network.allow_hosts,
  network.allow_private)?), harw_home::paths::cache_dir(home), WebFetchOptions{..})` aufrufen; sonst liefern alle drei
  Tools `NotConfigured` (RD-Ledger Z. 133/198 erwartet genau diesen Policy-Konstruktor).
- README `web.fetch`-Zeilen: Hinweis auf `configure`/Egress-Policy (Doku-Welle).

## 7. Annahmen / Entscheidungen (für Review)

1. **„finale URL“ im Schlüssel** = normalisierte Anfrage-URL (nur sie ist vor dem Abruf bekannt); das Endziel und alle
   Zwischenhops liegen in `chain` und werden bei jedem Treffer erneut geprüft (strenger als nur Endziel).
2. **„scope“** = Mandant ‖ Workspace ‖ Sandbox-`NetworkScope`-Ziele (keine Session-ID: Einträge sind innerhalb eines
   Workspace mit gleichem Scope sessionübergreifend teilbar).
3. **`http`** nur über `WebFetchOptions::allow_http` (Standard `false`); `EgressPolicy` hat keinen Schema-Schalter.
   Die Loopback-Tests nutzen `allow_http = true` + `allow_private = true`.
4. „Policy, die Loopback nur für den Starthost erlaubt“ ist mit `EgressPolicy` (globales `allow_private`) nicht
   ausdrückbar → Redirect-Tests auf Metadaten-IP (nie erlaubt) und Fremdhost, private-IP-Redirect als Hop-Funktionstest.
5. Modell-`max_bytes` kann nicht mehr über das konfigurierte Limit heben; Byte-Überschreitung bleibt Fehler (keine
   stille Kappung roher Bytes), Text wird gekappt.
6. `CacheCorrupt{path}` trägt bei Validierungsabweichung den Schlüssel-Hex statt eines Pfads; wird nur geloggt.
7. Cache-Verzeichnis-Eigentum wird über `chmod 0700` (scheitert bei Fremdbesitz) und Datei-UID-Prüfung beim Lesen
   abgesichert; keine `geteuid`-Dep in diesem Crate.
8. `rustfmt` nicht ausgeführt (Policy); einige Zeilen > 100 Zeichen → `cargo fmt` durch Orchestrator.
9. `harw-fsutil` ist Unix-only → `harw-tool-web` ebenfalls (Linux/Pi-Ziel).

## 8. Ausgabe

```json
{"agent":"N-WEB",
 "files_created":["/home/mia/projects/harwness/harw-tool-web/src/hop.rs","/home/mia/projects/harwness/harw-tool-web/src/cache.rs",
  "/home/mia/projects/harwness/harw-tool-web/src/html.rs","/home/mia/projects/harwness/docs/remediation/ledger/W5/N-WEB.md"],
 "files_modified":["/home/mia/projects/harwness/harw-tool-web/Cargo.toml","/home/mia/projects/harwness/harw-tool-web/src/fetch.rs",
  "/home/mia/projects/harwness/harw-tool-web/src/error.rs","/home/mia/projects/harwness/harw-tool-web/src/provider.rs",
  "/home/mia/projects/harwness/harw-tool-web/src/docs_rs.rs","/home/mia/projects/harwness/harw-tool-web/src/crates_io.rs",
  "/home/mia/projects/harwness/harw-tool-web/src/lib.rs"],
 "verification":{"command":"read-only; parent: cargo fmt -p harw-tool-web && cargo clippy -p harw-tool-web --tests -- -D warnings && cargo test -p harw-tool-web","exit_code":null,"pass":null},
 "stubbed_imports":[],
 "follow_up":["RD/W6 I-CONTRIB: harw_tool_web::configure(policy aus [network], harw_home::paths::cache_dir(home), options)"],
 "blocked":false}
```
