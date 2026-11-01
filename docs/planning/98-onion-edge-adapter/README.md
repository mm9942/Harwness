---
id: PL-98-ONION-EDGE-ADAPTER
title: "harw-edge: Rust-Dienste direkt an arti-Onion-Services und mTLS-HTTPS anschließen (ohne Reverse Proxy); CLI für systemd-Units und TOML mit Prüfung und Reparatur"
status: PROPOSED (Plan, nichts implementiert)
date: 2026-11-01
baseline_main: 78b22408
tags: [cloud, onion, tor, arti, mtls, tls, hyper, bytes, systemd, toml, clap, cloudflare, magic-dns, tailscale, edge]
related:
  - ../94-cloud-onboarding/README.md
  - ../89-reverse-proxy/README.md
  - ../89-reverse-proxy/EDGE-AUTH-LB-ENCRYPTION.md
  - ../linux-user-cloud-control/04-cloud-control-contract.md
  - ../linux-user-cloud-control/07-harw-cloud-integration.md
  - ../../design/tunnel-policy-v2-cloudflare.md
  - ../../architecture/dependency-review.md
  - ../70-decisions/README.md
  - 01-recherche-cloudflare-tor-arti.md
---

# PL-98 — harw-edge: Onion- und mTLS-Anschluss für Rust-Dienste, ohne Reverse Proxy

> **Status: PROPOSED.** Reiner Plan. Kein Crate, keine Abhängigkeit, keine Unit
> und kein Gate aus diesem Dokument existiert im Code. Nichts hier gilt als
> umgesetzt, bevor Code, Tests, Cargo-Metadaten und Doku nachgezogen sind
> (Regel aus [`../README.md`](../README.md) §1).
>
> Grundlage ist der Onion-Mechanismus von **comnet v2** (`comnet-transport`,
> Feature `arti`, und der apicon-Ingress). **Entscheidung des Owners
> (2026-11-01): harw-edge setzt als Adapter auf comnets Bootstrap-Element
> auf, das als installierbares Crate `comnet-arti` aus `comnet-transport`
> herausgelöst wird**; Harwness kopiert den arti-Code nicht (§4.4). comnet schaltet hinter die Onion einen
> Pingora-Proxy. Dieser Plan tut das **nicht**: Der Byte-Pfad ist
> `Dienst ⇄ TLS ⇄ arti-DataStream ⇄ Tor`, ohne Reverse Proxy, mit
> `bytes::Bytes` als Puffertyp. Die Recherche zu Cloudflare, Tor-Browser,
> Zertifikaten für `.onion` und dem arti-Stand steht mit Quellen in
> [`01-recherche-cloudflare-tor-arti.md`](01-recherche-cloudflare-tor-arti.md).

---

## 0. Kurzfassung

| Frage | Antwort dieses Plans |
|---|---|
| Was wird gebaut | Ein Adapter-Stack (`harw-edge-*`), der einen in Rust geschriebenen Dienst (hyper/tower-`Service` **oder** roher Byte-Stream-Handler) gleichzeitig auf drei Wegen erreichbar macht: ① HTTPS + mTLS auf IP/Hostname (eigene DNS, Tailscale-MagicDNS), ② Onion-Service über arti in-process mit TLS + mTLS **innerhalb** des Onion-Streams, ③ eine Cloudflare-verwaltete Domain, die auf ① zeigt und die Onion über `Onion-Location`, `Alt-Svc`, einen signierten `/.well-known/`-Datensatz und einen TXT-Record bekannt gibt. |
| Was wird **nicht** gebaut | Kein Pingora, kein Reverse Proxy, kein `cloudflared`, kein Tor-Exit-Verkehr, kein zweiter Adressklassifizierer, keine zweite Geräte-Registry, kein Hot-Reload im ersten Schnitt. |
| CLI | `harw-edge` (clap derive): `init`, `serve`, `onion`, `cert`, `unit`, `config`, `domain`, `doctor`. `unit` und `config` **prüfen und reparieren** systemd-Units und TOML-Dateien (Dry-Run mit Diff, Backup, atomarer Schreibpfad). |
| Grundlage | Das Bootstrap-Element von comnet als **installierbares Crate** `comnet-arti` (Owner-Entscheidung 2026-11-01; aus `comnet-transport` herausgelöst, comnet-seitiger Vertrag `comnet/docs/plans/arti-bootstrap-crate-contract.md`): `ArtiRuntime`, `OnionListener`, `ArtiDialer`, `TlsListener`, PQ-only-Provider, mTLS-Builder, `Conn`/`PeerInfo`. harw-edge ist der Adapter darüber: Dienst-Anschluss (hyper/`Vec<Bytes>`), CA/Leaf-Ausstellung, Domain-Bindung, CLI, Units. Nur comnet pinnt arti; Harwness pinnt `comnet-arti` exakt (§4.4). |
| Harte Voraussetzung | comnet und arti verlangen Rust 1.92 (comnet `Cargo.toml:20`, DEC-004). Harwness steht auf MSRV 1.89 (`Cargo.toml:218`), baut aber mit 1.98.1 (`rust-toolchain.toml:11`). Die MSRV muss angehoben werden (DEC-A). `comnet-arti` existiert noch nicht: der Split und die Veröffentlichung auf crates.io sind comnet-Arbeit (Vertrag oben, Entscheidungen U1–U7) und laufen **vor** W3. |
| Erste Lieferung | W0 (Spike + DECs) und W1 (`harw-unitfile`, `harw-edge-config`: reine Parser/Validierer/Reparierer, ohne Netz) sind sofort startbar und bringen schon ohne Tor Nutzen (PL-94 R1/R3: Units, die 18 400-mal am Tag neu starten, `README.md:37`). |

---

## 1. Ziel und Nicht-Ziele

### 1.1 Ziel

Ein Betreiber schreibt einen Dienst in Rust (Web-App, API, MCP-Server,
Session-Gateway) und bindet ihn mit **einer** Konfigurationsdatei und
**einer** CLI an:

1. **`direct`**: TLS 1.3 + X.509-mTLS auf einer IP oder einem Hostnamen.
   „Magic DNS“ meint hier den Tailscale-MagicDNS-Namen
   (`<host>.<tailnet>.ts.net`) oder einen eigenen DNS-Namen; der Adapter
   bindet an die Adresse, löst keine Namen selbst auf und vertraut dem Namen
   nicht als Identität (die Identität kommt aus dem Client-Zertifikat).
2. **`onion`**: arti in-process bootstrappt Tor, startet einen Onion-Service
   mit eigenem HS-Identity-Key, nimmt Rendezvous-Streams an und reicht jeden
   Stream durch **denselben** TLS/mTLS-Acceptor wie `direct` an den Dienst.
3. **`domain`**: eine bei Cloudflare verwaltete Domain zeigt per DNS auf
   `direct` (grey cloud, Ende-zu-Ende-mTLS) und kündigt die Onion an
   (`Onion-Location`, optional `Alt-Svc`, `/.well-known/harw-onion`, TXT).
   Der Tor Browser zeigt „.onion available“ an (keine automatische
   Umleitung mehr, Recherche F1.6); harw-Clients lernen die Onion aus dem
   signierten Datensatz, nicht aus DNS allein.

Alle drei Wege liefern dem Dienst dieselbe Schnittstelle: einen
`AsyncRead + AsyncWrite`-Stream nach TLS plus `PeerInfo` (Transport,
verifiziertes Client-Zertifikat als DER, ALPN, Onion-Nickname). Oben drauf
sitzt wahlweise hyper 1.x (HTTP/1.1, optional h2) oder ein roher
`Vec<Bytes>`-Handler.

### 1.2 Nicht-Ziele (bewusst)

- **Kein Reverse Proxy.** PL-89 bleibt eine getrennte Entscheidung
  (Pingora bringt ~240 Pakete, `zstd-sys`, `ring`; `89-reverse-proxy/README.md`
  §3). harw-edge dient **einen** Dienst pro Prozess und routet nicht.
- **Kein `cloudflared`.** Das ist `tunnel-policy-v2-cloudflare.md`, eine
  andere Risikoklasse (macht lokale Dienste öffentlich, Cloudflare terminiert
  TLS). Hier wird Cloudflare nur als **DNS- und Header-Anbieter** genutzt.
- **Kein Tor-Exit-Verkehr.** Der Dialer (falls mitgeliefert) wählt nur
  `.onion`-Ziele, wie comnet (`arti.rs:369-373`).
- **Keine eigene Identität aus Netz-Metadaten.** Quell-IP, Tailnet-Name,
  Hostname oder Onion-Herkunft sind nie Principal (Masterplan §14/§33, in
  `EDGE-AUTH-LB-ENCRYPTION.md` §2). Identität ist das verifizierte
  Client-Zertifikat; die Zuordnung zu Harw-Principals bleibt bei AuthHub bzw.
  `DeviceRegistry`.
- **Keine neue Geräte-Registry.** Zulassung von Client-Zertifikaten wird an
  `harw-node-listener::identity::DeviceRegistry` angebunden, nicht dupliziert
  (`07-harw-cloud-integration.md` §1, Zeile „Reuse this identity/storage path“).
- **Kein PQ-Versprechen für Tor selbst.** hs-ntor/ntor-v3 sind klassisch;
  kompensiert wird durch TLS 1.3 mit `X25519MLKEM768` **innerhalb** des
  Onion-Streams (comnet DEC-004, Abschnitt „Konsequenzen“).

---

## 2. Befundene Fakten (mit Belegen)

Stand `main@78b22408` (Harwness), comnet-Arbeitskopie vom 2026-11-01. Zeilen
können driften.

### 2.1 comnet: der Onion-Mechanismus, der portiert wird

| # | Befund | Beleg |
|---|---|---|
| C1 | `ArtiRuntime::bootstrap` installiert zuerst den rustls-Prozess-Default-Provider (arti baut sein Link-TLS daraus und würde sonst panicken), nimmt `TokioRustlsRuntime::current()`, baut `TorClientConfigBuilder::from_directories(state, cache)` mit optionaler Memory-Quota und wartet `create_bootstrapped()` ab. | `comnet/crates/comnet-transport/src/arti.rs:152-169`, `:116-127` |
| C2 | `launch_onion_on_port`: `HsNickname::new`, `OnionServiceConfigBuilder::default().nickname(..)`, `client.launch_onion_service(config)`; `Ok(None)` (Service deaktiviert) ist ein Fehler; die Adresse kommt aus `service.onion_address()` und wird mit `safelog::DisplayRedacted::display_unredacted` gerendert (HsId implementiert nur `DisplayRedacted`). Die Rendezvous-Requests laufen durch `tor_hsservice::handle_rend_requests`. | `arti.rs:215-259`; `comnet-transport/Cargo.toml` Feature `arti` (Kommentar zu `safelog`) |
| C3 | `OnionListener::accept`: nur `IncomingStreamRequest::Begin` auf **dem** Service-Port wird mit `Connected::new_empty()` angenommen; alles andere (`BEGIN_DIR`, `RESOLVE`, fremde Ports) wird mit `End::new_with_reason(EndReason::DONE)` abgelehnt, wie C-Tor, damit der Dienst nicht unterscheidbar ist. `PeerInfo` trägt `TransportKind::Arti` und den Nickname, nichts über den Client. Dropping des Listeners stoppt den Service (`Arc<RunningOnionService>`). | `arti.rs:272-319`, `:23-26` |
| C4 | `ArtiDialer::dial`: nur `.onion`-Ziele, `StreamPrefs::connect_to_onion_services(Explicit(true))`, Stream-Isolation aus einem 16-Byte-Isolationsschlüssel (`IsolationHelper`), Fehler nur als `error.kind()` (keine Adressen in Fehlern). | `arti.rs:343-387`, `:321-341` |
| C5 | Pins: `arti-client = "=0.47.0"` mit `default-features = false`, Features `tokio, rustls, onion-service-client, onion-service-service, compression, static-sqlite`; `tor-cell`, `tor-hsservice`, `tor-proto`, `tor-rtcompat` ebenfalls `=0.47.0`; `safelog = "=0.10.0"`. **Nie** `full` (zieht `anyhow`). `static-sqlite` bringt `libsqlite3-sys` (C-Build). arti 0.47 verlangt Rust 1.92. | `comnet-transport/Cargo.toml:10-19`; `comnet/docs/decisions/DEC-004-arti-exact-pin.md` |
| C6 | Der apicon-Ingress ist ein Pingora-`BackgroundService`: bootstrap → ein Onion pro `[[arti.onion]]` → pro Stream `OnionListener::accept` → `ArtiSock` (`VirtualSocket`) → `acceptor.tls_handshake` (**dieselbe** PQ-only-mTLS-Konfiguration wie TCP) → `HttpProxy::process_new`. Rate-Limit-Schlüssel pro Stream (SHA-256 aus Nickname + Zähler), weil `OnionListener` den Rendezvous-Circuit nicht preisgibt („reported gap“). | `comnet/crates/comnet-apicon/src/arti_ingress.rs:1-38`, `:181-227`, `:230-275` |
| C7 | TOML-Form: `[arti] state_dir, cache_dir` + `[[arti.onion]] nickname, app`; Validierung: absolute Pfade, ≥ 1 Onion, Nickname `1..=32` aus `[a-zA-Z0-9_-]`, eindeutig. | `comnet-apicon/src/config.rs:254-275`, `:475-495`, `:602-606` |
| C8 | mTLS-Verifier der Edge: WebPKI gegen Tenant-CA mit `allow_unauthenticated` (Zertifikatspflicht pro Route), Leaf muss genau eine `urn:…`-SAN tragen, SPKI-Hash gegen eine lock-frei gelesene Revocation-Menge. Resumption aus; jede Verbindung ein voller PQ-Handshake, damit kein Ticket Verbindungen über Circuit-Isolationsdomänen hinweg verknüpft. | `comnet-apicon/src/mtls.rs:1-44`; `comnet-transport/src/tls.rs:1-30` |
| C9 | Byte-Regel: öffentliche Daten, Frames, Bodies sind `bytes::Bytes` (`BytesMut`/`BufMut` zum Bauen, `Buf` zum Lesen); Klartext/Seeds/Schlüssel bleiben `Zeroizing<Vec<u8>>`, weil `Bytes` geteilt und nicht nullbar ist; Eingabeparameter `&[u8]`. | `comnet/CLAUDE.md` „Secrets, Ownership, Nebenläufigkeit“; `comnet/docs/decisions/DEC-041-bytes-migration-secret-boundary.md` |
| C10 | Transport-Abstraktion: `BoxConn = Box<dyn AsyncRead + AsyncWrite + Unpin + Send>` und `PeerInfo { kind, uid, gid, pid, remote, client_cert: Option<Vec<u8>>, alpn, onion_service }`. | `comnet-transport/src/conn.rs:34-47`, `:104-120` |
| C11 | Öffentliche Fläche von `comnet-transport`: `Listener`, `Dialer`, `Target`, `Isolation`, `TransportPolicy`, `PolicyDialer`, `TlsListener` (Handshakes nebenläufig, 10 s Timeout), `crypto_provider()` (PQ-only), `process_default_provider()` (hybrid zuerst, klassischer Fallback), `server_config(..)` mit optionalem `ClientCertVerifier`, `webpki_client_verifier(roots, allow_unauthenticated)`, `load_pem_chain`/`load_pem_key`; Features `direct`, `local`, `arti`, jede Kombination baut. Einzige interne Abhängigkeit: `comnet-proto` (nur `getrandom`, `sha2`, `subtle`, `zeroize`). | `comnet-transport/src/lib.rs:1-37`, `src/tls.rs:136-560`; `comnet-proto/Cargo.toml:9-13` |
| C12 | comnet ist `publish = false`, Workspace-Version `0.1.0`, `rust-version = "1.92"`. comnet erlaubt sich selbst einen Git-Pin nur befristet und per DEC (crypt_guard, DEC-003). | `comnet/Cargo.toml:17-21`; `comnet/docs/decisions/DEC-003-crypt-guard-git-rev.md` |

### 2.2 Harwness: was es schon gibt und was fehlt

| # | Befund | Beleg |
|---|---|---|
| H1 | `harw-cloud` ist ein Aggregator über `harw-cloud-enroll`, `harw-cloud-gateway` (YAML-Konfig `listenAddr/tls/ports`, `proxy.rs` ausdrücklich Platzhalter, Pingora nur als optionales Feature), `harw-cloud-ops`, `harw-cloudctl`. „not a complete deployed cloud service“. | `harw-cloud/src/lib.rs`, `harw-cloud/README.md`, `harw-cloud/harw-cloud-gateway/src/config.rs:1-43`, `harw-cloud-gateway/Cargo.toml` |
| H2 | Der Cloud-Vertrag verlangt: Legacy-YAML nur als explizite validierte Migration in **ein** normalisiertes Konfigmodell; Readiness in getrennten Stufen (configured, dependency reachable, peer authenticated, protocol compatible, service mounted, scope bound, restrictions available, admission accepting); TCP-Annahme beweist keine Stufe. | `linux-user-cloud-control/04-cloud-control-contract.md` §2 Nr. 6, §4 |
| H3 | Inter-Node-TLS: rustls 0.23 + aws-lc-rs, nur TLS 1.3, nur `X25519MLKEM768`, ephemerer Ed25519-Raw-Public-Key (RFC 7250), **keine X.509-Zertifikate, kein Client-Auth** (`with_no_client_auth` an beiden Stellen). Harwness hat heute **nirgends** einen `ClientCertVerifier`. | `harw-node-transport/src/tls.rs:1-16`, `:42-47`, `:64`, `:85`; `rg ClientCertVerifier` leer |
| H4 | Edge-Plan: zwei TLS-Profile (Inter-Node PQ-only vs. Browser-Edge mit klassischem X.509); Pingora-rustls nutzt `ring` und TLS 1.2/1.3 und bietet **kein** `X25519MLKEM768`; Edge-Key-Custody ist offen (`HarwKeyPurpose` kennt keinen TLS-Zweck). | `89-reverse-proxy/EDGE-AUTH-LB-ENCRYPTION.md` §1 |
| H5 | `harw-reverse-proxy` ist eine reine Policy-Bibliothek ohne Sockets; `harw-egress` liefert `classify`, `EgressPolicy`, `build_client` (reqwest 0.12 mit gefiltertem Resolver) und einen SOCKS5-Proxy über Unix-Socket. | `harw-reverse-proxy/src/lib.rs:1-20`; `harw-egress/src/lib.rs:1-60` |
| H6 | `harw-tailscale`: Adressprüfung (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`), LocalAPI-`whois`, Gate fail-closed, Proxy auf Tailnet-Adresse → dedizierter Unix-Socket. Kein MagicDNS-Code. | `harw-tailscale/src/lib.rs:1-32` |
| H7 | systemd heute: `harw-install` rendert eine **User**-Unit mit `Type=simple`, `ExecStart`, `WorkingDirectory`, `Environment`, `Delegate=yes`, `Restart=always`, **ohne Hardening**; `exec.join(" ")` ohne Quoting. Die kanonischen System-Units in `deploy/systemd/` sind dagegen voll gehärtet (`ProtectSystem=strict`, `SystemCallFilter=@system-service`, `CapabilityBoundingSet=`, `RestrictAddressFamilies=…`). `harw-netsec::systemd` prüft `LISTEN_PID`/`LISTEN_FDS` explizit (Socket-Aktivierung ohne `unsafe`). | `harw-install/src/service_systemd.rs:381-406`; `deploy/systemd/harw-security-hub.service`; `harw-netsec/src/systemd.rs:1-60` |
| H8 | TOML-Reparaturmuster existiert: `harw-config::writer::ConfigWriter` editiert mit `toml_edit` formaterhaltend, validiert **vor** dem Schreiben (fail closed), rotiert Backups (`MAX_BACKUPS = 5`), schreibt atomar über `harw_fsutil`, sperrt per Sidecar-Datei. `toml_edit 0.25.15` ist im Lock. | `harw-config/src/writer.rs:1-45`; `Cargo.lock` |
| H9 | Feldbefunde PL-94: handgeschriebene Unit mit 18 400 Neustarts/Tag, Crash-Loop durch tote Provider-Konfig, Cloudflare-Tunnel ohne `cert.pem`, Node „über mTLS/HTTPS anbinden, aber nie kompilieren“. Daraus R1 (`node doctor` prüft Unit-Gesundheit), R2 (thin node), R3 (generierte, überwachte Unit mit Back-off). | `94-cloud-onboarding/README.md:37-51` |
| H10 | Architektur-Gate: jedes neue Paket braucht einen `[packages."…"] layer`-Eintrag in `xtask/arch-policy.toml`; Netz- und Cloud-Crates stehen in Ring **A**; `[[forbidden_crates]]` kann Crates für Ringe sperren (Vorbild: `tungstenite` nur Ring A). `*-sys` sind nur in J und TCB verboten. | `xtask/arch-policy.toml:1-30`, `:234-240`, `:405-452`, `:500-525` |
| H11 | `deny.toml`: `openssl*`, `native-tls` verboten; nur crates.io als Quelle (`unknown-git = deny`); jede Advisory-Ausnahme mit Begründung. Ein Git-Pin wie comnets crypt_guard-Rev wäre hier **nicht** zulässig. | `deny.toml:80-92` |
| H12 | Workspace-Versionen im Lock: `rustls 0.23.46`, `hyper 1.11.1`, `toml 1.1.3` (workspace), `toml_edit 0.25.15`, `clap 4.6.1` (derive, in sechs Binaries), `tokio-rustls 0.26.x`, `aws-lc-rs 1.18.1`, `pingora-core 0.9.0` (optional). `rcgen`, `x509-parser`, `arti-client` sind **nicht** im Lock. | `Cargo.lock`; `harw-node-transport/Cargo.toml`; `harw-cli/Cargo.toml:110` |
| H13 | Lint-Vertrag: `unsafe_code = "forbid"` workspaceweit; einzelne Crates setzen `deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)` und `warn(missing_docs)` selbst. Build nur über `make clippy-tests` / `make gates`; Worker bauen nie (DEC-004 Harwness). | `Cargo.toml:263-266`; `harw-infra-client/src/lib.rs:40-45`; `Makefile:45-51`; `70-decisions/DEC-004-no-parallel-builds.md` |
| H14 | Egress-Verträge: jede URL durch `EgressPolicy::check_url`, Redirects manuell, Resolver filtert Adressklassen. Ein Cloudflare-API-Client muss diesen Pfad nutzen (`build_client`), kein eigener reqwest. | `harw-egress/src/lib.rs:36-44` |

### 2.3 Was daraus folgt (Lücken)

1. Es gibt in Harwness **keinen** Onion-Code und **keine** X.509-mTLS-Fläche; beides ist neu.
2. Der arti-Pfad erzwingt **MSRV 1.92** und bringt `libsqlite3-sys` (C-Build) in den Baum; beides braucht einen DEC-Record und eine Gate-Prüfung (Ring A ist erlaubt, J/TCB bleiben frei).
3. Unit-Dateien werden heute an zwei Stellen in zwei Qualitäten erzeugt (H7); ein gemeinsamer, prüfbarer Unit-Typ fehlt. Genau das ist der Platz für `harw-unitfile`.
4. Die Cloud-Gateway-Konfiguration ist YAML mit `unsafe-libyaml`-Advisory-Ausnahme (`deny.toml`); der Vertrag will TOML. harw-edge liefert das normalisierte TOML-Modell, in das die YAML migriert.

---

## 3. Recherche-Ergebnis in einem Absatz

Details und Quellen: [`01-recherche-cloudflare-tor-arti.md`](01-recherche-cloudflare-tor-arti.md).
Für den Plan entscheidend:

- DNS kann **nicht** auf eine Onion zeigen (`.onion` ist nach RFC 7686 eine
  Special-Use-Domain; autoritative Server müssen NXDOMAIN liefern, F1.9).
  „Eine Cloudflare-Domain, die nach Tor zeigt“ heißt deshalb: die Domain
  zeigt per `A`/`AAAA` auf den `direct`-Listener, und die **Antwort**
  kündigt die Onion an: `Onion-Location`-Header (Tor Browser zeigt seit
  13.0.12 nur noch eine „.onion available“-Pille, keine automatische
  Umleitung, F1.5/F1.6), optional `Alt-Svc` mit `.onion`-Authority und ein
  TXT-Record als harw-Konvention (es gibt keinen Standard, F1.10).
- `Alt-Svc` auf die eigene Onion funktioniert nur, wenn der Onion-Stream ein
  für die **Clearnet-Domain** gültiges, öffentlich vertrauenswürdiges
  Zertifikat präsentiert (RFC 7838, F1.8). Mit `client_auth.mode =
  "required"` hat der Tor Browser ohnehin kein Client-Zertifikat; `Alt-Svc`
  ist deshalb nur in Profilen mit `optional`/`none` sinnvoll.
- Cloudflares „Onion Routing“ betreibt Cloudflares **eigene** Onions vor dem
  Origin und terminiert TLS bei Cloudflare; API-Shield-mTLS reicht
  Client-Zertifikate dem Origin nur als Header weiter (F1.1, F1.14). Damit
  ist Ende-zu-Ende-mTLS zum Dienst nicht möglich. Für mTLS bleibt der Record
  **DNS-only (grey cloud)**; Onion Routing plus eine Transform Rule, die
  `Onion-Location` auf die **eigene** Onion setzt (F1.7), ist nur für
  öffentliche, nicht mTLS-geschützte Inhalte ein Komfort (`domain-orange`).
- Öffentlich vertrauenswürdige Zertifikate für `.onion` gibt es nach den
  CA/B-Forum-Regeln (Anhang B) nur von wenigen CAs, z. B. HARICA (DV);
  Let's Encrypt stellt keine aus (F1.12). harw-edge braucht sie nicht: der
  Onion-Stream trägt TLS mit der **eigenen** harw-CA, und harw-Clients
  pinnen die CA. Für Tor-Browser-Nutzer ohne Client-Zertifikat ist eine
  Onion-DV-CA ein späteres Thema (§12, O7).
- arti `0.47.0` ist **Arti 2.7.0 vom 2026-10-01 und die aktuelle Version**
  (F2.1); comnets Pin ist aktuell, MSRV 1.92 (F2.2). Restricted Discovery
  ist auf der Service-Seite **nicht mehr experimentell** (F2.6) und wird
  Profil `onion-restricted`; PoW ist experimentell mit offenen Bugs (F2.7)
  und bleibt draußen. `OnionServiceConfigBuilder` kennt
  `rate_limit_at_intro` und `max_concurrent_streams_per_circuit` (F2.5);
  comnet-transport setzt heute nur `nickname`, beides wird als
  Feature-Request an comnet gestellt (§4.4 c). Single Onion Services gibt es
  in arti nicht (F2.8). Der offizielle Status von artis Onion-Services ist
  „testing and experimentation only“ (F2.11); das steht als Risiko in §12.

---

## 4. Architektur

### 4.1 Der Byte-Pfad (ohne Reverse Proxy)

```text
                 ┌──────────────── harw-edge Prozess ───────────────────────────┐
  Tor-Netz ──►  arti TorClient ──► RunningOnionService ──► handle_rend_requests  │
                 │                 (HS-Key im StateDirectory)        │          │
                 │                                   StreamRequest (BEGIN :443)  │
                 │                                             ▼                 │
  IP/Hostname ─► TcpListener (direct) ──────────────► BoxConn (AsyncRead+Write)  │
  (MagicDNS,     │                                             ▼                 │
   eigene DNS)   │                       tokio-rustls Acceptor (ein ServerConfig │
                 │                         für alle Wege: TLS 1.3, mTLS-Verifier)│
                 │                                             ▼                 │
                 │                     TlsStream + PeerInfo { kind, client_cert,│
                 │                                             alpn, onion }     │
                 │                           ┌─────────────────┴──────────────┐  │
                 │                           ▼                                ▼  │
                 │             hyper 1.x serve_connection            StreamHandler│
                 │             (TokioIo, http1 / h2, tower::Service)  (Vec<Bytes>)│
                 │                           ▼                                ▼  │
                 │                      Der Rust-Dienst (Web-App, API, MCP, …)    │
                 └────────────────────────────────────────────────────────────────┘
```

Regeln:

1. **Ein** `ServerConfig` für alle Wege (wie comnet C6: „so mTLS works over
   the onion too“). Unterschiedliche Wege bekommen unterschiedliche
   `PeerInfo`, nie unterschiedliche Vertrauensregeln.
2. Der Onion-Weg akzeptiert nur `BEGIN` auf dem konfigurierten virtuellen
   Port (Standard 443) und lehnt alles andere mit `END DONE` ab (C3).
3. `Bytes` ist der Puffertyp **nach** TLS für öffentliche Daten (C9): Der
   `StreamHandler` liest in ein `BytesMut`, friert Frames per
   `split_to(n).freeze()` ein und liefert `Vec<Bytes>`; Antworten werden als
   `Vec<Bytes>` angenommen und per `write_all_buf` über einen
   `bytes::Buf`-Chain geschrieben, ohne Kopie. Klartext-Geheimnisse
   (Schlüssel, Tokens) kommen **nie** in `Bytes`; sie bleiben in
   `zeroize::Zeroizing<Vec<u8>>`. hypers interne Puffer sind außerhalb
   dieser Zusicherung (so wie `harw-infra-client` es dokumentiert).
4. Es gibt **keinen** Fallback zwischen den Wegen: ein nicht gestarteter
   Onion-Service lässt `direct` laufen und meldet `onion = unavailable`
   mit Grund (Stufenmodell H2), er leitet nichts um.

### 4.2 Crate-Schnitt

| Crate | Ring | Inhalt | Externe Abhängigkeiten (neu kursiv) |
|---|---|---|---|
| `harw-unitfile` | **F** | Reiner systemd-Unit-Parser (`systemd.syntax(7)`: Sektionen, `Key=Value`, Fortsetzungszeilen mit `\`, Kommentare `#`/`;`, Mehrfachschlüssel, Leerzuweisung als Reset), Direktiven-Tabelle (`[Unit]`, `[Service]`, `[Socket]`, `[Install]`, Exec-Direktiven aus `systemd.exec(5)`), Typprüfung (bool, Zeitspanne, Pfad, Capability-Liste, Syscall-Set), Hardening-Lint mit Score, formaterhaltende Reparatur, Renderer. Keine Prozesse, kein I/O. | keine |
| `harw-edge-config` | **I** | TOML-Schema `edge.toml` (§8), `Deserialize` mit `deny_unknown_fields`, `validate()`, `toml_edit`-Reparatur (Schlüssel-Vorschläge per Levenshtein gegen das Schema, fehlende Pflichtfelder mit Platzhalter + Kommentar, Typkorrekturen nur wo eindeutig), Diff-Erzeugung, Backup + atomares Schreiben über `harw-fsutil`. | `toml`, `toml_edit` (beide im Lock), `serde`, `harw-fsutil` |
| `harw-edge-tls` | **A** | X.509-Profil **über** `comnet_arti::tls`: eigene Edge-CA (Ed25519 **oder** ECDSA P-256, DEC-C), Leaf-Ausstellung mit SAN-Pflicht (`DNS:`/`IP:` für Server, `URI:urn:harw:device:<id>` für Clients), `ClientCertVerifier` als Hülle um comnets `webpki_client_verifier` (+ genau eine Pflicht-SAN + SPKI-Revocation-Set), Provider-Profile `pq-strict` = `comnet_arti::tls::crypto_provider()` und `pq-preferred` = `comnet_arti::tls::process_default_provider()` (hybrid zuerst, klassischer Fallback für Browser), Resumption aus (comnet-Default), PEM/Credential-Laden. **Voraussetzung für `pq-preferred`:** comnets heutiges `server_config` baut fest auf `crypto_provider()`, und `TlsListener::new` lehnt jede andere Provider-Konfiguration beim `accept` mit `ProviderMismatch` ab (`comnet-transport/src/tls.rs:406-417`, `:560`, `:603`). `comnet-arti 0.1.0` muss deshalb `server_config_with_provider(..)` und `TlsListener::new_with_provider_policy(.., ProviderPolicy::HybridPreferred)` mitbringen (comnet-Vertrag §2.1, U5); W2 nimmt `pq-preferred` nur mit einem Handshake-Test eines klassischen X25519-Clients ab. | *`comnet-arti`* (Feature `direct`), *`rcgen`*, *`x509-parser`*, `rustls`, `rustls-pki-types` |
| `harw-onion` | **A** | **Adapter** über `comnet-arti` mit Feature `arti`: re-exportiert `ArtiConfig`, `ArtiRuntime`, `OnionListener`, `ArtiDialer`, `IsolationKey`; implementiert `harw_edge_serve::EdgeListener` für `comnet_arti::Listener`-Typen und bildet `comnet_arti::PeerInfo` (C10) auf `harw_edge_serve::PeerInfo` ab (`client_cert: Vec<u8>` → `CertificateDer`, SPKI-Hash berechnen, Peer-Key ableiten). Enthält **keinen** arti-Code. Feature `restricted-discovery` sobald `comnet-arti 0.2` es anbietet (Vertrag U5); PoW nie. | *`comnet-arti`* (Features `direct`, `arti`; arti-Pin und `safelog` kommen transitiv, DEC-B) |
| `harw-edge-serve` | **A** | `Listener`-Trait (`direct`, `onion`, `uds`), `TlsAcceptorLayer` (Handshake-Timeout 10 s, begrenzte parallele Handshakes), `serve_http(listener, service)` über hyper 1.x `http1::Builder` (+ `http2` optional) mit `hyper_util::rt::TokioIo`, `serve_stream(listener, handler)` für `Vec<Bytes>`-Handler, Verbindungslimits, Peer-Key für Rate-Limits (pro Stream, C6-Lücke dokumentiert), `PeerInfo` als `http::Extensions`-Eintrag. Pro Weg `tokio::select!` mit Shutdown-Watch. | `hyper`, `hyper-util`, `tower-service`, `http`, `http-body-util`, `bytes`, `tokio` |
| `harw-edge-domain` | **A** | Domain-Bindung: `Onion-Location`/`Alt-Svc`-Middleware (nur bei `https`-Request mit passendem `Host`), `/.well-known/harw-onion` (JSON, mit ML-DSA-65 über AuthHub signiert, Felder: `domain`, `onion`, `issued_at`, `expires_at`, `ca_spki_sha256`), TXT-Record-Konvention `_harw-onion.<domain>` = `v=harw1; onion=<56 chars>.onion; spki=<b64>`, Client-seitige Verifikation (Signatur **und** Domain-Match). Optional (Feature `cloudflare-api`): DNS-Records über die Cloudflare-API mit `harw_egress::build_client`, Token nur als Dateireferenz, nie in argv/Logs (Regel 4 aus `tunnel-policy-v2`). | `harw-egress`, `harw-infra-client` (Signer), `serde_json` |
| `harw-edge-cli` → Binary `harw-edge` | **A** | clap derive, Unterbefehle §9, `tracing` mit `--log`, `ExitCode`. Nutzt `harw-install::service::ServiceManager` für User-Units und schreibt System-Units nur als Datei (Installation durch den Operator oder `--system` mit explizitem Pfad), nie `systemctl enable` implizit (Vertrag §2 Nr. 2: Lebenszyklus ≠ Enrollment). | `clap 4.6.1`, alle oben |

Begründung für Ring A bei `harw-onion`/`harw-edge-tls`/`harw-edge-serve`:
Netz-, TLS- und Cloud-Crates stehen heute in A (H10); arti (über
`comnet-arti`) bringt `libsqlite3-sys` und wäre in J/TCB verboten. `harw-unitfile` als **F** ist
Absicht: harw-install (A), harw-netsec (A), PL-94 `node doctor` und die
DoD-Installer sollen denselben Parser nutzen können; ein F-Crate ohne
Abhängigkeiten ist dafür die einzige Stelle, die alle erreichen.

### 4.3 Beziehung zu bestehenden Crates

- `harw-install::service::ServiceSpec`/`render_unit` bleibt API-stabil;
  intern rendert `service_systemd.rs` künftig über `harw_unitfile::UnitDocument`
  (korrektes Quoting von `ExecStart`, kein `join(" ")`). Das ist W1-Teil
  „Adoptionspfad“, keine Pflicht für die erste Lieferung.
- `harw-cloud-gateway::GatewayConfig` (YAML) bekommt eine Funktion
  `migrate_to_edge_toml(&GatewayConfig) -> EdgeConfig` (explizite Migration,
  H2). Die YAML-Abhängigkeit wird erst danach entfernt (eigene Änderung,
  wie der `deny.toml`-Kommentar es verlangt).
- `harw-cloud-ops::status` meldet pro Weg die Readiness-Stufen (H2) plus
  `onion_address: Option<String>` (öffentlich, darf geloggt werden) und
  `restricted_discovery: bool`.
- `harw-reverse-proxy` wird **nicht** eingebunden; wer später Pingora will,
  setzt ihn **vor** `direct`, nie in den Onion-Pfad.
- `harw-tailscale` bleibt der Weg „Tailnet ohne öffentliche Ports“; harw-edge
  `direct` kann an die Tailnet-Adresse binden (dann gilt zusätzlich das
  bestehende `TailnetGate`, Entscheidung pro Konfiguration, §8 `direct.bind`).

### 4.4 Kopplung an comnet (Owner-Entscheidung: Adapter, keine Kopie)

harw-edge **konsumiert** comnets Bootstrap-Element (künftig `comnet-arti`,
heute Teil von `comnet-transport`) und baut nichts davon nach.
Das hat drei Seiten:

**a) Was Harwness aus comnet nimmt (C11), künftig aus `comnet-arti`:**
`Listener`/`Dialer`/`BoxConn`/`PeerInfo`, `TlsListener`,
`ArtiRuntime`/`OnionListener`/`ArtiDialer`/`IsolationKey`,
`crypto_provider()`, `process_default_provider()`, `install_default_provider()`,
`server_config(..)`, `webpki_client_verifier(..)`, `load_pem_chain`/`load_pem_key`.
Damit entfallen in Harwness: der arti-Pin, der Provider-Code, der
TLS-Listener mit Handshake-Begrenzung und der WebPKI-Client-Verifier.
Harwness ergänzt nur, was comnet nicht hat: Dienst-Anschluss über
hyper/`Vec<Bytes>`, CA/Leaf-Ausstellung, SAN-/Revocation-Hülle,
Domain-Bindung, CLI, Units, TOML-/Unit-Reparatur.

**b) Bezugsweg (DEC-G): entschieden, installierbares Crate.** Der User hat
am 2026-11-01 festgelegt, dass das Bootstrap-Element als **installierbare
Abhängigkeit** für den Harw-Adapter bereitsteht. comnet ist heute
`publish = false` (C12) und Harwness erlaubt nur crates.io
(`deny.toml:90-92`); also wird veröffentlicht, und zwar **nicht** ganz
`comnet-transport`, sondern ein herausgelöstes Leaf-Crate:

| Weg | Was zu tun ist | Status |
|---|---|---|
| **G1b `comnet-arti`** (gewählt) | comnet löst aus `comnet-transport` das Leaf-Crate `comnet-arti` heraus: `arti.rs`, `conn.rs`, `listener.rs`, `dialer.rs`, `tls.rs`, `direct.rs`, eigener `BoxFuture`-Alias, eigenes Fehler-Enum ohne `ToErrorCode`, `IsolationKey([u8; 16])` statt comnets `Isolation`; `comnet-transport` re-exportiert alles unter den heutigen Pfaden; nur `comnet-arti` wird `publish = true`. Harwness pinnt `comnet-arti = "=0.1.0"` (exakte Version nach Cargo-Syntax `=MAJOR.MINOR.PATCH`; jeder Release-Bump ist eine eigene, reviewte Änderung). Vertrag mit Impact-Liste und Entscheidungen U1–U7: `comnet/docs/plans/arti-bootstrap-crate-contract.md`. | Entwurf auf der comnet-Seite, wartet auf U1–U7 und auf den grünen M2-Build |
| G1a ganz `comnet-transport` + `comnet-proto` veröffentlichen | nur `publish = true` setzen | Verworfen: `comnet-proto` trägt Wire-Typen, Fehlercodes und `App`, die Harwness nichts angehen; die Isolation aus Principal × `App` ist comnet-Semantik |
| G2 Git-Rev befristet | `deny.toml` `allow-git`, volle Rev, Ablaufdatum | Nur als **Spike-Hilfe** in W0 (Scratch-Crate außerhalb des Repos), nie im Harwness-Baum |
| G3 Pfad-Abhängigkeit | – | Nein: zwei Repos, zwei Lockdateien |

**c) Rückwirkung auf comnet (Pflicht laut comnet `CLAUDE.md`, „App-Grenzen
… vor jeder Änderung prüfen“).** Mit Harwness als Konsument wird die
`comnet-transport`-API ein Vertrag mit einer Außenseite:

- Jede Änderung an `Listener`, `Dialer`, `PeerInfo`, `TlsListener`, den
  TLS-Buildern oder an `arti.rs` listet den Impact künftig **auch für
  Harwness** auf (neben apicon, service-kit, ①②③, client, ffi, Fuzz).
  Vorschlag: eine Zeile „externe Konsumenten: Harwness `harw-onion`,
  `harw-edge-tls`“ in `comnet/docs/plans/crate-api-contract.md` §11 und ein
  Contract-Test in Harwness, der die genutzten Signaturen festnagelt.
- Der arti-Pin (`=0.47.0`, DEC-004) und sein Monatsrhythmus gelten damit
  **für beide Repos**: comnet bumpt, Harwness zieht mit `make clippy-tests`
  nach. Harwness bekommt keinen eigenen arti-Pin (DEC-B wird zu „Pin folgt
  comnet“).
- Features, die harw-edge zusätzlich braucht (Restricted Discovery, PoW,
  Circuit-Kennung für Rate-Limits, HS-Key-Export), werden als
  **Feature-Requests an `comnet-arti`** gestellt, mit Impact-Liste, und
  dort umgesetzt, nicht in Harwness nachgebaut (Vertrag U5: `0.2.0`).
- `comnet-arti` bleibt frei von `harw-*`-Abhängigkeiten (Richtung nur
  Harwness → comnet). comnets Edge-Gates (G1–G7) sind davon unberührt, weil
  Harwness außerhalb des comnet-Workspace liegt; comnets Verbot von
  `anyhow`/`thiserror` im eigenen Code bleibt, Harwness-Altcrates mit
  `thiserror` sind für comnet unsichtbar.
- MSRV: Harwness übernimmt comnets `rust-version = "1.92"` (DEC-A). Hebt
  comnet die MSRV (arti-Bump), hebt Harwness im selben Zug.

**d) Typ-Brücke.** comnets `PeerInfo.client_cert` ist `Option<Vec<u8>>`
(C10), `BoxFuture` kommt aus `comnet-proto`. `harw-onion` baut daraus den
Harwness-`PeerInfo` (§7) mit `CertificateDer`, SPKI-Hash und Peer-Key; die
Dienste oberhalb sehen **nur** Harwness-Typen. So kann G1/G2 später
gewechselt werden, ohne dass Dienste etwas merken.

---

## 5. Erreichbarkeitsprofile (das „neue Konzept“ für harw-cloud)

| Profil | Weg | Wer kommt rein | Wofür |
|---|---|---|---|
| `tailnet` | `direct` an Tailnet-IP, mTLS **plus** `TailnetGate` | eigene Geräte im Tailnet mit Client-Cert | Standardempfehlung aus PL-94 („the tunnel is the tailnet“) |
| `public-mtls` | `direct` an öffentliche IP / eigenen DNS-Namen, mTLS Pflicht, `pq-strict` oder `pq-preferred` | Geräte mit Client-Cert | Thin Nodes, Phones ohne Tailscale, Partnerstandorte |
| `onion-mtls` | `onion` (arti in-process), TLS + mTLS **im** Stream | Geräte mit Client-Cert, die die `.onion` kennen | kein offener Port, kein DNS, NAT-frei, Standort verborgen |
| `onion-restricted` | wie `onion-mtls` **plus** Restricted Discovery (Client-Keys im Descriptor) | nur Geräte mit registriertem x25519-Key **und** Client-Cert | Hub-Verwaltung, Break-Glass-Zugang |
| `domain` | `direct` (grey cloud) + Onion-Ankündigung (`Onion-Location`, `Alt-Svc`, `.well-known`, TXT) | wie `public-mtls`; Tor Browser wird zur Onion geleitet | ein merkbarer Name, der beide Welten verbindet |
| `domain-orange` | Cloudflare-Proxy mit „Onion Routing“ (Cloudflares Onions) **plus** Transform Rule `Onion-Location` → eigene Onion, **vor** einem Dienst **ohne** mTLS | Öffentlichkeit | nur statische/öffentliche Inhalte; **nicht** für Hub-APIs (Cloudflare terminiert TLS, F1.14) |

Profile sind **Konfigurationsvorlagen** für `edge.toml` (§8), kein eigener
Code-Pfad. `harw-edge init --profile onion-mtls` schreibt die passende
Vorlage. Mehrere Profile gleichzeitig sind erlaubt (`direct` + `onion` ist der
Normalfall für `domain`).

---

## 6. Sicherheitsmodell

### 6.1 Was welcher Baustein leistet

| Baustein | Schützt vor | Schützt **nicht** vor |
|---|---|---|
| Onion-Service | Standortaufdeckung, Port-Scans, DNS-Abhängigkeit, Zensur des Namens | unautorisierten Clients (jeder, der die Adresse kennt, erreicht den TLS-Handshake), Harvest-now-decrypt-later auf der Tor-Schicht (klassisch) |
| Restricted Discovery | Fremden, die die Adresse erraten/finden: ohne registrierten Key kein Descriptor | Kompromittiertem Client-Key, Missbrauch durch zugelassenes Gerät |
| TLS 1.3 `X25519MLKEM768` | Mitlesen und Aufzeichnen des Inhalts, auch gegen Quantenangreifer (hybrid) | Identitätsbetrug ohne mTLS |
| mTLS mit eigener CA + Revocation-Set | Unautorisierten Clients auf **allen** Wegen; Widerruf wirkt beim nächsten Handshake | Kompromittiertem Server-Host (dort liegen HS-Key und TLS-Key) |
| Signierter `.well-known`-Datensatz | Gefälschter Onion-Ankündigung per DNS-Manipulation (Signatur mit AuthHub-Schlüssel, Domain im Signat) | Erstkontakt ohne vorher bekannten Signaturschlüssel (TOFU-Grenze, dokumentieren) |

### 6.2 Schlüsselverwahrung

- **HS-Identity-Key**: liegt in artis Keystore unter `StateDirectory=harw-edge`
  (`/var/lib/harw-edge/arti/state/keystore/…`), Modus `0700`, Eigentümer der
  Dienstnutzer; arti prüft Rechte selbst (fs-mistrust, C1). arti hat **kein**
  Export-Kommando (F2.10); `harw-edge onion export --to <file>` kopiert
  deshalb die Keystore-Dateien `hss/<nickname>/ks_hs_id` (OpenSSH-Format)
  mit `0600` und ausdrücklichem Hinweis, dass die Datei die Onion-Identität
  **ist**. `onion import` geht programmatisch über
  `launch_onion_service_with_hsid` (F2.4; `comnet-arti` Feature
  `onion-import`, Vertrag §2.1) oder über artis read-only C-Tor-Keystore.
- **TLS-Server-Key**: über systemd `LoadCredential=tls-key:<pfad>` in
  `$CREDENTIALS_DIRECTORY`, gelesen über eine Deskriptor-Prüfung (comnet
  `load_tls_key`-Muster, kein zweites Öffnen des Pfads). Alternativ Datei mit
  `0600`.
- **Edge-CA-Key**: **nicht** auf dem Edge-Host. Zwei Varianten, Entscheidung
  DEC-C: (a) CA-Key im AuthHub mit neuem `HarwKeyPurpose::EdgeCa`
  (Signieren von Leaf-TBS-Strukturen, Rate-Limit, Audit), (b) Offline-CA auf
  dem Operator-Gerät, Leafs per `harw-edge cert issue --ca-key <file>` und
  Verteilung per Datei. Der Edge-Host hält nur Root-Zertifikat + Revocation-Set.
- **Cloudflare-API-Token**: nur als Dateireferenz (absoluter Pfad), nie in
  argv, Logs, Fehlern, Units (Regel aus `tunnel-policy-v2` §2 Nr. 4).

### 6.3 Logging- und Fehlerregeln

- Onion-Adresse ist **öffentlich** und darf im `info`-Log stehen (comnet
  `arti_ingress.rs:209-210`).
- Nie loggen: Client-Zertifikat (DER/PEM), SPKI im Klartext (nur gekürzter
  Hash), Isolationsschlüssel, Pfade von Credentials, Request-Bodies.
- arti-Fehler nur als `error.kind()` bzw. safelog-redigierte Nachricht (C4).
- Ablehnungen im Onion-Accept (`BEGIN_DIR`, fremder Port) werden bei `debug`
  gezählt, nicht einzeln mit Metadaten geloggt.

### 6.4 Lastschutz

- Verbindungslimit pro Weg, Handshake-Timeout 10 s, begrenzte parallele
  Handshakes (comnet `MAX_PENDING_HANDSHAKES`-Muster).
- Rate-Limit-Schlüssel pro Onion-Stream (SHA-256 aus Nickname + Zähler) ist
  eine **bekannte Schwäche** (C6): mehrere Streams eines Circuits teilen
  keinen Schlüssel. arti 0.47 bietet `max_concurrent_streams_per_circuit`
  und `rate_limit_at_intro` (F2.5); beide werden über
  `comnet_arti::OnionServiceOptions` (Vertrag §2.1, ab `0.2.0`) in
  `edge.toml` `[onion]` verfügbar und schließen die
  Lücke auf Circuit-Ebene. PoW bleibt aus (experimentell, F2.7).
- `direct`: Quell-IP nur für Vor-Authentisierungs-Limits, nie für Identität.

---

## 7. Öffentliche API-Skizze (Rust)

```rust
// harw-edge-serve — der Anschluss für Dienste.
pub trait EdgeListener: Send {
    fn accept(&mut self) -> BoxFuture<'_, Result<(BoxConn, PeerInfo)>>;
    fn describe(&self) -> String;                      // "direct:[::]:8443", "onion:hub:443"
}

#[non_exhaustive]
pub struct PeerInfo {
    pub kind: TransportKind,                            // Direct | Onion | Uds
    pub remote: Option<std::net::SocketAddr>,           // nur Direct
    pub client_cert: Option<rustls::pki_types::CertificateDer<'static>>,
    pub client_spki_sha256: Option<[u8; 32]>,
    pub alpn: Option<bytes::Bytes>,
    pub onion_service: Option<String>,                  // Nickname, nicht die Adresse
    pub peer_key: [u8; 16],                             // Rate-Limit-Schlüssel
}

/// HTTP-Dienst anschließen (hyper 1.x). `PeerInfo` landet in `req.extensions()`.
pub async fn serve_http<S, B>(
    listeners: Vec<Box<dyn EdgeListener>>,
    tls: Arc<rustls::ServerConfig>,
    service: S,
    shutdown: tokio::sync::watch::Receiver<bool>,
    limits: ServeLimits,
) -> Result<ServeReport>
where
    S: tower_service::Service<http::Request<hyper::body::Incoming>, Response = http::Response<B>>
        + Clone + Send + 'static,
    S::Future: Send, S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    B: http_body::Body<Data = bytes::Bytes> + Send + 'static, B::Error: Into<Box<dyn std::error::Error + Send + Sync>>;

/// Roher Byte-Dienst: Frames als `Vec<Bytes>` hinein, `Vec<Bytes>` hinaus.
pub trait StreamHandler: Send + Sync + 'static {
    fn on_connect(&self, peer: &PeerInfo) -> Result<Option<Vec<bytes::Bytes>>>;  // Begrüßung
    fn on_frames(&self, peer: &PeerInfo, frames: Vec<bytes::Bytes>) -> BoxFuture<'_, Result<Reply>>;
}
pub enum Reply { Send(Vec<bytes::Bytes>), SendAndClose(Vec<bytes::Bytes>), Close }

pub async fn serve_stream<H: StreamHandler>(
    listeners: Vec<Box<dyn EdgeListener>>,
    tls: Arc<rustls::ServerConfig>,
    framing: Framing,            // LengthPrefixedU32 { max } | Delimited { delim, max } | RawChunks { max }
    handler: Arc<H>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    limits: ServeLimits,
) -> Result<ServeReport>;

// harw-onion — Adapter über comnet-arti (kein eigener arti-Code).
pub use comnet_arti::{ArtiConfig, ArtiRuntime, ArtiDialer, IsolationKey, OnionListener};

/// Hülle, die einen comnet-arti-Listener (OnionListener, DirectListener)
/// als Harwness-EdgeListener anbietet und dessen PeerInfo in Harwness-PeerInfo übersetzt.
pub struct ComnetListener<L: comnet_arti::Listener>(L);
impl<L: comnet_arti::Listener> EdgeListener for ComnetListener<L> { /* accept → map PeerInfo */ }

pub async fn launch_onion(rt: &Arc<ArtiRuntime>, nickname: &str, port: u16)
    -> Result<(OnionAddress, ComnetListener<OnionListener>)>;      // BEGIN :port → accept; sonst END DONE (comnet)

// harw-edge-tls — über comnet_transport::tls.
pub fn server_config(profile: KxProfile, cert_chain: &Path, key: KeySource, client_auth: ClientAuth)
    -> Result<Arc<rustls::ServerConfig>>;     // PqStrict → comnet_arti::tls::crypto_provider() + TlsListener::new;
                                              // PqPreferred → comnet_arti::tls::process_default_provider() über
                                              // server_config_with_provider + TlsListener::new_with_provider_policy(HybridPreferred)
pub enum KxProfile { PqStrict, PqPreferred }
pub enum ClientAuth { Required { roots: Vec<CertificateDer<'static>>, revoked: Arc<RevocationSet> },
                      Optional { .. }, None }
```

Fehler: ein handgeschriebenes Enum pro Crate (`EdgeError`, `OnionError`,
`UnitfileError`, `EdgeConfigError`), `Display` klein und englisch, `source()`
gesetzt, `From` pro Fremdfehler, **keine** `thiserror`/`anyhow` in neuem Code
(wie comnet; Harwness erlaubt `thiserror` in Altcrates, neue Crates folgen der
strengeren Regel, Begründung in DEC-D).

---

## 8. Konfiguration `edge.toml`

Pfad: `/etc/harw/edge/edge.toml` (System) oder `~/.harw/edge.toml` (User).
Schema (`deny_unknown_fields`, alle Pfade absolut):

```toml
# harw-edge — Erreichbarkeit eines Dienstes. `harw-edge config check` prüft,
# `harw-edge config repair` repariert formaterhaltend.
schema = 1
service = "hub"                       # Dienstname: Unit-Name, Nickname-Vorgabe, Log-Feld

[tls]
cert_chain = "/etc/harw/edge/chain.pem"   # Leaf der Edge-CA (harw-Clients) oder öffentliche Kette
                                          # (Let's Encrypt via DNS-01 / `tailscale cert`) für Browser
key = { credential = "tls-key" }      # oder { file = "/etc/harw/edge/tls.key" }
kx_profile = "pq-strict"              # "pq-strict" | "pq-preferred"
alpn = ["h2", "http/1.1"]             # Reihenfolge = Präferenz; leer = kein ALPN

[tls.client_auth]
mode = "required"                     # "required" | "optional" | "none"
roots = "/etc/harw/edge/edge-ca.pem"
revocations = "/var/lib/harw-edge/revoked.spki"   # eine SPKI-SHA-256 (hex) je Zeile
required_san = "urn:harw:device:"     # Präfix der Pflicht-SAN im Leaf

[direct]
enabled = true
bind = ["[::]:8443"]                  # oder Tailnet-IP; mehrere erlaubt
tailnet_gate = false                  # true: zusätzlich harw-tailscale TailnetGate
max_connections = 512

[onion]
enabled = true
state_dir = "/var/lib/harw-edge/arti/state"
cache_dir = "/var/cache/harw-edge/arti"
nickname = "hub"                      # 1..=32 aus [a-zA-Z0-9_-], eindeutig pro Keystore
virtual_port = 443
memquota_bytes = 268435456
num_intro_points = 3                  # arti-Default; max 20 (F2.5)
rate_limit_at_intro = [25, 200]       # [rate/s, burst] als DOS_PARAMS an Intro-Points (F2.5)
max_streams_per_circuit = 64          # arti max_concurrent_streams_per_circuit (F2.5)
restricted_discovery = { enabled = false, key_dirs = ["/etc/harw/edge/onion-clients"] }
                                      # Dateien <name>.auth mit "descriptor:x25519:<base32>" (F2.6)
max_connections = 256

[domain]
enabled = false
name = "hub.example.org"              # Host, auf den direct per DNS zeigt (grey cloud)
announce_onion_location = true        # Header nur bei https + Host == name
announce_alt_svc = true
well_known_signing_key = "harw.edge-binding/hub"   # AuthHub-Schlüsselname
txt_record = true                     # _harw-onion.<name> schreiben (nur mit cloudflare.api)

[domain.cloudflare]
api_token_file = "/etc/harw/edge/cloudflare.token"   # nur Dateireferenz
zone_id = "…"
proxied = false                       # false = grey cloud (Pflicht für mTLS)

[serve]
mode = "http"                         # "http" | "stream"
handshake_timeout_ms = 10000
max_pending_handshakes = 64
[serve.stream]                        # nur mode = "stream"
framing = { length_prefixed_u32 = { max_frame_bytes = 1048576 } }

[log]
level = "info"
```

Validierungsregeln (Auszug, alle als Tests): `schema == 1`; `service` wie
Nickname-Regel; mindestens ein Weg `enabled`; `kx_profile = "pq-strict"` mit
`client_auth.mode = "none"` wird **abgelehnt** (öffentlicher Dienst ohne
Identität, aber Browser-inkompatibel: Konfigurationsfehler, kein Default);
`domain.cloudflare.proxied = true` mit `client_auth.mode != "none"` wird
abgelehnt (mTLS endet sonst bei Cloudflare); `onion.nickname` eindeutig je
`state_dir`; `bind` keine Cloud-Metadaten-/Multicast-Adressen
(`harw_egress::classify`); Pfade absolut; `txt_record = true` braucht
`[domain.cloudflare]`.

Reparaturklassen (`harw-edge config repair`):

| Klasse | Beispiel | Aktion |
|---|---|---|
| R-Key | `[tls] cert_chian` | Vorschlag `cert_chain` (Levenshtein ≤ 2 gegen Schema), Umbenennen mit Kommentar `# repaired: cert_chian -> cert_chain` |
| R-Missing | `[tls]` ohne `cert_chain` | Einfügen `cert_chain = ""  # TODO: required` → Datei bleibt **ungültig**, Exit 3, aber vollständig editierbar |
| R-Type | `virtual_port = "443"` | Umwandeln in `443`, wenn eindeutig parsebar |
| R-Deprecated | `[gateway]` aus YAML-Migration | Umhängen nach `[direct]`, Alt-Tabelle als Kommentar erhalten |
| R-Unsafe | `proxied = true` + mTLS | **keine** automatische Reparatur; Fehlertext mit beiden Optionen |

Jede Reparatur: `--dry-run` zeigt Unified-Diff, `--write` legt
`edge.toml.bak.<n>` an (max. 5) und schreibt atomar (H8-Muster).

---

## 9. CLI `harw-edge` (clap derive)

```text
harw-edge [--config PATH] [--log LEVEL] <command>

  init      --profile <tailnet|public-mtls|onion-mtls|onion-restricted|domain>
            [--service NAME] [--bind ADDR]... [--write]           Vorlage schreiben (sonst stdout)
  serve     [--systemd-socket]                                     Dienst betreiben (ExecStart)
  onion     address | export --to FILE | import --from FILE | rotate --confirm
  cert      ca init --out DIR | issue --kind server|client --san ... --ca DIR
            | revoke --spki HEX | list
  unit      write [--system|--user] [--out DIR] [--socket]          Units erzeugen (+ .socket für :443 ohne Caps)
            | check FILE...  | repair FILE... [--dry-run|--write] [--level strict|standard]
            | lint FILE...  (Hardening-Score, Begründung je Direktive)
  config    check [FILE] | repair [FILE] [--dry-run|--write] | migrate-yaml FILE
  domain    show | announce-test URL | dns plan | dns apply --confirm
  doctor    [--json]   Readiness-Stufen je Weg, Unit-Zustand, Key-Rechte, Onion-Bootstrap-Status
```

Regeln:

- `--log <LEVEL>` an jedem Aufruf; Subscriber nur im Binary installiert
  (`tracing_subscriber`, `try_init`), Bibliotheken installieren keinen.
- Exit-Codes: `0` ok, `2` Benutzungsfehler (clap), `3` Konfiguration
  ungültig, `4` Prüfung fehlgeschlagen (check/lint), `5` Laufzeitfehler;
  `main` gibt `ExitCode` zurück, kein `process::exit`.
- `unit write` erzeugt für `--system` **gehärtete** Units nach dem Vorbild
  `deploy/systemd/harw-security-hub.service` (§10); für `--user` nutzt es
  `harw-install::ServiceManager`. Es ruft **nie** `systemctl enable` oder
  `start` ohne eigenes, explizites Unterkommando (`harw-cloudctl` bleibt der
  Lebenszyklus-Adapter, H1).
- `unit check` ist rein (Parser + Direktiven-Tabelle + Typen). `unit lint`
  bewertet Hardening (Score 0–100 mit Begründung je fehlender Direktive).
  Der eigene Prüfer ist **strenger** als systemd: eine unbekannte Direktive
  ist ein Fehler, systemd selbst warnt nur und lädt die Unit (F3.5). Wenn
  `systemd-analyze` installiert ist, bietet `doctor` zusätzlich
  `systemd-analyze verify --man=no --recursive-errors=yes` (ohne root) und
  `systemd-analyze security --offline=yes --json=short` (ohne PID 1) als
  **externe** Zweitmeinung an (F3.1, F3.2); die eigene Prüfung ist davon
  unabhängig.
- `unit repair --level standard` ergänzt fehlende Pflichtstruktur
  (`[Unit]`/`[Service]`/`ExecStart`), korrigiert Direktivnamen (Levenshtein),
  normalisiert Booleans (`true` → `yes`), quotet `ExecStart`-Argumente nach
  `systemd.syntax(7)`; `--level strict` ergänzt außerdem den
  Hardening-Block aus §10 (nur Direktiven, die den Dienst nicht brechen:
  keine `SystemCallFilter`-Änderung an fremden Units ohne `--force`).
- `domain dns apply` ist die **einzige** Aktion mit Fernwirkung und verlangt
  `--confirm`; `dns plan` zeigt vorher die Record-Änderungen.

---

## 10. systemd-Units (Vorlage, die `unit write --system` erzeugt)

```ini
# harw-edge@.service -- harw-edge for one service (instance = service name).
[Unit]
Description=harw-edge %i (direct + onion ingress)
Documentation=man:harw-edge(1)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=notify                               # sd_notify READY=1 nach Bootstrap aller aktivierten Wege
NotifyAccess=main
User=harw-edge
Group=harw-edge
ExecStart=/usr/bin/harw-edge --config /etc/harw/edge/%i.toml --log info serve
Restart=on-failure
RestartSec=5
TimeoutStartSec=180                       # Tor-Bootstrap kann Dutzende Sekunden dauern (C1)
StateDirectory=harw-edge/%i
StateDirectoryMode=0700
CacheDirectory=harw-edge/%i
CacheDirectoryMode=0700
LoadCredential=tls-key:/etc/harw/edge/%i.key
UMask=0077

# -- Hardening --
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
ProtectKernelModules=yes
ProtectKernelTunables=yes
ProtectKernelLogs=yes
ProtectControlGroups=yes
ProtectClock=yes
ProtectHostname=yes
ProtectProc=invisible
ProcSubset=pid
RestrictNamespaces=yes
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LockPersonality=yes
MemoryDenyWriteExecute=yes
RestrictRealtime=yes
RestrictSUIDSGID=yes
RemoveIPC=yes
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallErrorNumber=EPERM
CapabilityBoundingSet=
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
```

- Port 443 auf `direct`: **kein** `CAP_NET_BIND_SERVICE`, sondern eine
  `harw-edge@.socket`-Unit (`ListenStream=443`) und `serve --systemd-socket`
  mit der expliziten `LISTEN_PID`/`LISTEN_FDS`-Prüfung aus `harw-netsec`
  (H7). Die Onion braucht keinen Port und keine Capability.
- `Type=notify` ist Teil der Readiness (H2): `READY=1` erst, wenn jeder
  aktivierte Weg annimmt; `STATUS=` trägt die Stufen (`direct: ready;
  onion: bootstrapping 63%`). Ohne libsystemd: `sd_notify` über das
  `NOTIFY_SOCKET`-Datagramm in reinem Rust (kein `unsafe`, kein `-sys`).
- Bewusst **kein** `DynamicUser=`: arti prüft Verzeichnisrechte selbst und
  der HS-Key braucht einen stabilen Eigentümer; die von `DynamicUser=`
  implizierten Härtungen (`NoNewPrivileges`, `RestrictSUIDSGID`,
  `RemoveIPC`, `ProtectSystem=strict`, F3.3) stehen explizit in der Unit.
  Kein `PrivateNetwork=` (trennt alle Socket-Familien, F3.3).
- Die Vorlage selbst wird mit `harw-unitfile` geparst und gelintet; sie ist
  ein Fixture der eigenen Tests (Score ≥ 90). Die kanonischen Units in
  `deploy/systemd/` sind der Golden-Korpus des Parsers (alle müssen ohne
  Befund parsen).

---

## 11. Domain-Bindung im Detail

1. **DNS (Cloudflare, grey cloud):** `A`/`AAAA hub.example.org → direct-IP`,
   `proxied = false`. Orange cloud ist mit mTLS unvereinbar (Cloudflare
   terminiert TLS; Recherche F1 in `01-recherche…`).
2. **Antwort-Header auf `direct` (nur `https`, nur `Host == domain.name`):**
   `Onion-Location: https://<onion>.onion<path>` (Spec: HTTPS-Seite, kein
   Onion-Origin, F1.5). Der Tor Browser zeigt „.onion available“ und
   wechselt erst auf Klick (F1.6). `Alt-Svc: h2="<onion>.onion:443";
   ma=86400; persist=1` nur, wenn `tls.cert_chain` öffentlich
   vertrauenswürdig **für die Domain** ist und `client_auth.mode !=
   "required"` (F1.8); sonst setzt harw-edge den Header **nicht** und
   `config check` meldet das. Beide Header sind reine Ankündigung; der
   Dienst **erzwingt** nichts.
3. **Signierter Datensatz** `GET /.well-known/harw-onion`:
   `{"v":1,"domain":"hub.example.org","onion":"….onion","ca_spki_sha256":"…","issued_at":…,"expires_at":…,"sig":{"alg":"ML-DSA-65","key":"harw.edge-binding/hub","value":"…"}}`.
   Signiert über AuthHub (`harw-infra-client`), damit der Signaturschlüssel
   nie auf dem Edge-Host liegt. harw-Clients akzeptieren eine Onion für
   eine Domain nur, wenn Signatur gültig **und** `domain` gleich **und**
   nicht abgelaufen. Ablauf ≤ 30 Tage; Rotation durch `domain announce`.
4. **TXT-Konvention** `_harw-onion.<domain>`: `"v=harw1; onion=<56>.onion; spki=<b64 32 Byte>"`.
   Dient nur dem **Auffinden**; Vertrauen kommt aus 3. Es gibt keinen
   Standard dafür; das Tor-Projekt hält DNS-Discovery ausdrücklich für
   optional und erwägt HTTPS/SVCB-Records (F1.10). Der Plan erklärt die
   Konvention als harw-eigen, hält sie klein und sieht einen Wechsel auf
   einen künftigen Standard-RR vor.
5. **Zertifikate für die Onion:** Der Onion-Weg präsentiert dieselbe
   Zertifikatskette wie `direct`. Mit der eigenen Edge-CA: SAN
   `DNS:hub.example.org` **und** `DNS:<onion>.onion`; harw-Clients pinnen
   die Edge-CA, der Tor Browser warnt. Mit öffentlicher Kette (Let's
   Encrypt für die Domain): Browser akzeptieren `direct`, und `Alt-Svc`
   wird möglich (2.); die `.onion`-SAN fehlt dann (Let's Encrypt stellt
   keine aus, F1.12). Eine Onion-DV-CA (HARICA) oder SANs nach Proposal 361
   „OASIC“ (F1.13, noch ohne Implementierungen) sind §12, O7.

---

## 12. Offene Punkte (Entscheidungen des Owners)

| # | Frage | Empfehlung |
|---|---|---|
| O1 | MSRV 1.89 → 1.92 (arti). Betrifft alle Crates, nicht nur harw-edge. | Anheben; die Toolchain ist 1.98.1, Dependabot pflegt sie. DEC-A. |
| O2 | arti-Version | **Entfällt als eigene Frage**: Harwness erbt comnets Pin (`=0.47.0`, DEC-004) über `comnet-arti`. Ein Bump passiert in comnet (Minor-Bump von `comnet-arti`); Harwness zieht nach (§4.4 c). |
| O3 | `pq-strict` als Default? Browser (auch Tor Browser) können `X25519MLKEM768`-only verweigern, je nach Version. | Default `pq-strict` für `client_auth.mode = "required"` (nur harw-Clients), `pq-preferred` sonst. `pq-preferred` setzt die provider-konfigurierbare Listener-API in `comnet-arti 0.1.0` voraus (§4.2, comnet-Vertrag U5); kommt sie nicht, fällt `pq-preferred` aus dem Ziel und Browser-Profile brauchen einen separaten Edge. |
| O4 | Edge-CA-Key im AuthHub (neuer `HarwKeyPurpose`) oder offline? | AuthHub, weil Revocation/Audit dort sind; offline als Übergang. DEC-C. |
| O5 | Kurve der Edge-CA: Ed25519 (klein, schnell; rustls-Clients ok, Browser-Support gemischt) oder ECDSA P-256 (überall)? | P-256 für Server-Leafs, Ed25519 nur, wenn ausschließlich harw-Clients. In `edge.toml` wählbar. |
| O6 | **Entschieden (Owner, 2026-11-01): Adapter auf comnet, keine Kopie; das Bootstrap-Element wird ein installierbares Crate (`comnet-arti`, G1b).** Offen sind nur noch die comnet-seitigen Detailfragen U1–U7 im Vertrag `comnet/docs/plans/arti-bootstrap-crate-contract.md` (Name, Schnitt, Publish-Weg, Lizenz, Zeitpunkt). | Schnitt B (mit TLS), Name `comnet-arti`, Trusted Publishing, erst nach grünem M2-Build. |
| O7 | Öffentliche DV-Zertifikate für die `.onion` (HARICA o. ä.) für Tor-Browser-Nutzer ohne Client-Cert? | Später; erst wenn ein öffentliches Profil ohne mTLS gebraucht wird. |
| O8 | Restricted Discovery in der ersten Lieferung? | Nur, wenn der Spike zeigt, dass die gepinnte arti-Version es ohne Zusatz-Feature-Explosion kann; sonst W5. |
| O9 | `harw-unitfile` als F-Crate: darf `harw-install` (A) sofort darauf umgestellt werden? | Ja, aber als eigener PR nach W1, mit Golden-Test, dass `render_unit` byte-gleich bleibt, außer Quoting-Fixes. |
| O10 | Soll `harw-edge` ein eigenes Binary bleiben oder Unterbefehl `harw edge` werden? | Eigenes Binary (thin nodes ohne `harw`, PL-94 R2); `harw-cli` kann später delegieren. |
| O11 | arti stuft seine Onion-Services offiziell als „testing and experimentation only“ ein (F2.11); comnet trägt das Risiko bereits (DEC-004 „Konsequenzen“). Für harw-cloud akzeptieren? | Ja, mit derselben Kompensation wie comnet (TLS 1.3 PQ + mTLS im Stream); `onion` nie als **einziger** Weg für Hub-Verwaltung ohne `direct`-Fallback über Tailnet. |
| O12 | Welche Erweiterungen werden bei `comnet-arti` beantragt: `OnionServiceOptions` (`rate_limit_at_intro`, `max_concurrent_streams_per_circuit`, `num_intro_points`, `restricted_discovery`) und `import_onion_identity` (Import)? | Im comnet-Vertrag bereits als `0.2.0` vorgesehen (U5); `0.1.0` ist der reine Split. Bis `0.2.0` laufen `onion-mtls` ohne Rate-Knobs und `onion-restricted` **nicht**. |
| O13 | Server-Zertifikat im Profil `tailnet`: `tailscale cert` (öffentlich, Maschinenname landet in CT-Logs, F5.2) oder Edge-CA-Leaf (nur harw-Clients)? | Edge-CA-Leaf als Default; `tailscale cert` nur, wenn Browser im Tailnet den Dienst öffnen sollen, mit Hinweis auf CT. |

---

## 13. DEC-Records, die dieser Plan braucht

| DEC | Inhalt |
|---|---|
| DEC-A | MSRV `rust-version = "1.92"` (arti); Prüfung aller Crates; CI-MSRV-Job. |
| DEC-B | arti kommt **nur** transitiv über `comnet-arti` (Pin, Feature-Liste ohne `full`, Bump-Rhythmus und Security-Watch gehören comnet DEC-004). Harwness-seitig: `libsqlite3-sys` in Ring A, nie J/TCB; `[[forbidden_crates]]`: `comnet-arti`, `arti-client`, `tor-*` nur über `harw-onion`/`harw-edge-tls`. |
| DEC-G | Bezugsweg: `comnet-arti` von crates.io, exakt gepinnt (`=0.1.0`; jeder Bump auf eine konkrete neue Version ist eine eigene, reviewte Änderung, kein Wildcard), Minor-Bump je arti-Bump; Contract-Test gegen die genutzte API; Rückwirkung auf comnets Impact-Liste (§4.4 c). Gegenstück auf comnet-Seite: DEC-042-Entwurf im Vertrag. |
| DEC-C | Edge-CA-Custody (AuthHub-Purpose `EdgeCa` oder offline), Leaf-Laufzeiten (Server 90 Tage, Client 365 Tage), Revocation-Set-Format. |
| DEC-D | Fehlerstil neuer Crates: handgeschriebene Enums ohne `thiserror`/`anyhow`; `Bytes` für öffentliche Puffer, `Zeroizing` für Geheimnisse (Übernahme comnet DEC-041 für harw-edge-Crates). |
| DEC-E | Domain-Bindung: `/.well-known/harw-onion`-Format, TXT-Konvention, Signaturschlüssel-Name, Ablauf, TOFU-Grenze. |
| DEC-F | Zwei TLS-Profile (`pq-strict`/`pq-preferred`) und ihre erlaubten Kombinationen mit `client_auth`. |

Alle Records nach `docs/planning/70-decisions/` im dortigen Format
(YAML-Frontmatter; Entscheidung → Warum → Folgen → Wo im Code).

---

## 14. Abhängigkeiten und Gates

| Abhängigkeit | Version | Status | Bemerkung |
|---|---|---|---|
| `comnet-arti` (Features `direct`, `arti`, später `restricted-discovery`) | `=0.1.0` von crates.io (exakt; Bump nur als eigene, reviewte Änderung) | **neu**, Crate existiert noch nicht | Bringt arti `=0.47.0` samt `tor-*`, `safelog`, `libsqlite3-sys` transitiv; kein `comnet-proto`; `cargo tree -i anyhow`, `-i ring`, `-i openssl-sys`, `-i libsqlite3-sys` im Spike protokollieren; `deny.toml` bleibt unverändert (crates.io) |
| `rcgen` | 0.14.10 (MSRV 1.88, F4) | **neu** | CA/Leaf-Erzeugung; Backend `aws_lc_rs` (kein zweiter Provider); nur in `harw-edge-tls` und CLI |
| `x509-parser` | 0.18.1 (F4) | **neu** | SAN-Prüfung im Verifier (comnet nutzt dafür `certinfo`); rcgen `from_ca_cert_der` |
| `hyper-util` Features `server-auto`, `service` | 0.1.x (Lock) | vorhanden | `auto::Builder` (HTTP/1.1 oder h2 pro Verbindung) und `TowerToHyperService` ersetzen eigenen Adapter-Code (F4) |
| `rustls`, `tokio-rustls`, `aws-lc-rs`, `hyper`, `hyper-util`, `tower-service`, `http*`, `bytes`, `tokio` | wie im Lock | vorhanden | keine neue Fassung |
| `toml`, `toml_edit`, `serde`, `clap` | wie im Lock | vorhanden | `toml_edit` ist heute nur transitiv/pfadweise gelistet; direkt in `[workspace.dependencies]` eintragen |
| `cargo deny` | – | – | neue Lizenzen prüfen (arti-Baum: MIT/Apache, `safelog`), erwartete Advisory-Ausnahmen: keine |
| `xtask gates arch` | – | – | sieben neue `[packages]`-Einträge, eine neue `[[forbidden_crates]]`-Regel (DEC-B) |

Eintragsregel: Versionen kommen vom Orchestrator nach Prüfung gegen den
Registry-Index in die Manifeste; Worker schlagen vor. Die Lockdatei wird
mit dem kanonischen Build aktualisiert, nie mit `cargo add`.

---

## 15. Arbeitspakete (Wellen)

Disjunkte Dateimengen pro Worker; Verifikation ausschließlich zentral über
`make clippy-tests` (Harwness DEC-004/DEC-005). Jede Welle ist ein PR.

| Welle | Lieferung | Dateien (Owner) | Abnahme |
|---|---|---|---|
| **W0** Spike + DECs | Scratch-Crate außerhalb des Repos mit `comnet-transport` per Git-Rev (nur im Spike): Baum messen (`anyhow`, `ring`, `openssl`, `*-sys`, Paketzahl), Bootstrap-Zeit, `TlsListener` über `OnionListener`, Restricted Discovery in arti 0.47, Lizenzen. DEC-A…G schreiben. MSRV-Bump. **Parallel auf comnet-Seite:** U1–U7 entscheiden, `comnet-arti` herauslösen und `0.1.0` veröffentlichen (Vertrag `arti-bootstrap-crate-contract.md`). | Harwness: `docs/planning/70-decisions/DEC-*.md`, `Cargo.toml` (MSRV), `docs/planning/98-…/02-spike-report.md`; comnet: eigener PR nach dem Vertrag | Spike-Report mit Messwerten; `make clippy-tests` nach MSRV-Bump grün; `cargo deny check` grün; `comnet-arti 0.1.0` auf crates.io sichtbar und MSRV 1.92 gemerged (Voraussetzung für W2 **und** W3) |
| **W1** Reine Prüfer | `harw-unitfile` (Parser, Direktiven-Tabelle, Typen, Lint, Repair, Render) und `harw-edge-config` (Schema, Validierung, Repair, Diff, Backup/atomar). Golden-Korpus `deploy/systemd/*`. | `harw-unitfile/**`, `harw-edge-config/**`, `xtask/arch-policy.toml` (2 Einträge) | Alle Units in `deploy/systemd/` parsen ohne Befund; Fuzz-Targets für beide Parser; Repair-Dry-Run ist idempotent (zweiter Lauf: leerer Diff); jede Reparaturklasse §8 hat einen Test |
| **W2** TLS/mTLS (**braucht `comnet-arti 0.1.0` mit `server_config_with_provider` + `ProviderPolicy`**) | `harw-edge-tls`: Provider-Profile, CA/Leaf (rcgen), Verifier (WebPKI + Pflicht-SAN + Revocation-Set), PEM/Credential-Laden, Resumption aus. | `harw-edge-tls/**`, `arch-policy.toml` | Loopback-Tests: Client ohne Cert → `required` lehnt ab / `optional` lässt durch mit `client_cert = None`; widerrufene SPKI → Handshake-Fehler; `pq-strict` lehnt einen Client mit nur klassischem `X25519` ab; **`pq-preferred` nimmt denselben klassischen Client an und handelt mit einem hybriden Client `X25519MLKEM768` aus** (Browser-Simulation über rustls-Client mit eingeschränkten `kx_groups`); Debug-Ausgaben enthalten kein Schlüsselmaterial |
| **W3** Onion + Serve (**blockiert bis `comnet-arti 0.1.0` auf crates.io**) | `harw-onion` (Adapter über `comnet-arti`: `ComnetListener`, PeerInfo-Brücke, `launch_onion`, Contract-Test gegen die genutzte comnet-API) und `harw-edge-serve` (Listener-Trait, TLS-Layer, hyper- und Stream-Pfad, Limits, Shutdown). | `harw-onion/**`, `harw-edge-serve/**`, `arch-policy.toml` | Duplex-/UDS-Tests ohne Netz für Serve (HTTP/1.1, h2, Stream-Framing, Limits, Shutdown); PeerInfo-Brücke (DER → SPKI-Hash, Peer-Key) getestet; `#[ignore]`-Smoke-Test mit echtem Tor (`dial(own onion)` ↔ `accept`), wie comnet; Fehlerpfade `Ok(None)`, fehlender HS-Key, fremder Port kommen typisiert aus comnet durch |
| **W4** CLI + Units | `harw-edge-cli`: alle Unterbefehle außer `domain`; Unit-Vorlagen (§10), `--systemd-socket`, `sd_notify`, `doctor`. | `harw-edge-cli/**`, `deploy/systemd/harw-edge@.service`, `harw-edge@.socket`, `deploy/sysusers.d`, `deploy/tmpfiles.d`, `Makefile` (Ziel `edge-install`) | `unit lint` der eigenen Vorlage ≥ 90; `config check` auf allen `init`-Profilen grün; `doctor --json` liefert die Stufen aus H2; CLI-Tests mit `clap`-Parser (`try_parse_from`), kein Prozessstart in Unit-Tests |
| **W5** Domain | `harw-edge-domain`: Header-Middleware, `.well-known`-Datensatz (Signatur via AuthHub), TXT-Konvention, Client-Verifikation; Feature `cloudflare-api` (plan/apply, Token-Datei, `harw_egress::build_client`). `harw-edge domain …`. | `harw-edge-domain/**`, `harw-edge-cli/src/domain_cmd.rs` | Header nur bei `https` + passendem Host; Signaturprüfung schlägt bei fremder Domain/abgelaufen fehl; `dns plan` zeigt Diff, `apply` ohne `--confirm` tut nichts; Token nie in Debug/Fehlern |
| **W6** Cloud-Integration + Doku | `harw-cloud-ops::status` mit Wegen/Stufen; `GatewayConfig → EdgeConfig`-Migration; `harw-cloud/README.md`, `docs/planning/94-cloud-onboarding` (R1/R3 erfüllt durch `harw-unitfile`/`unit write`), `07-harw-cloud-integration.md` §3 um HC-Zeile ergänzt; `harw-install::service_systemd` auf `harw-unitfile` (O9). | `harw-cloud/**`, `harw-install/src/service_systemd.rs`, Doku | Migrationstest YAML→TOML; `cloud.status`-Schema v3 mit nullbaren Stufen; Golden-Test `render_unit` |

Reihenfolge: W0 → W2 → W3 → W4 → W5 ‖ W6, mit W1 parallel zu W0 und W2.
Nur **W1** ist von arti, `comnet-arti` und der MSRV unabhängig und kann
vor dem MSRV-Bump beginnen (reine Parser, keine neue Abhängigkeit). **W2**
hängt an `comnet-arti 0.1.0` (`rust-version = 1.92`) und damit an W0:
Manifest und Build von `harw-edge-tls` sind vor dem veröffentlichten Crate
und dem MSRV-Bump nicht möglich. Vor W0 darf aus W2 nur die
abhängigkeitsfreie Vorarbeit laufen (CA-/Leaf-Profil, SAN-Regeln,
Revocation-Set-Format als Doku und Testvektoren in `docs/planning/98-…/`),
kein Crate.

---

## 16. Tests (Übersicht)

- **Unit/Contract (ohne Netz):** Unit-Parser-Golden-Korpus; Repair-Idempotenz;
  TOML-Reparaturklassen; TLS-Loopback (tokio `duplex`); Serve über
  `duplex` und UDS; Framing-Grenzen (`max_frame_bytes + 1` → typisierter
  Fehler, Verbindung geschlossen); Shutdown-Watch beendet Accept-Schleifen;
  `PeerInfo` in `Request::extensions`.
- **Fuzz (`fuzz/`, kein Workspace-Member):** `harw-unitfile::parse`,
  `harw-edge-config::repair`, `.well-known`-Datensatz-Parser, Stream-Framing.
- **Smoke (`#[ignore]`, braucht Netz, wie comnet):** arti-Bootstrap, Onion
  starten, eigene Onion wählen, TLS + mTLS im Stream, HTTP-Roundtrip.
- **Gates:** `xtask gates arch` (Ringe, forbidden_crates), `cargo deny`,
  `make clippy-tests` als einziges Verifikationskommando.
- **Negativ-Tests, die Pflicht sind:** Null-/leere Nickname, Nickname 33
  Zeichen, relative Pfade, `proxied = true` + mTLS, `pq-strict` + `none`,
  Client-Cert ohne Pflicht-SAN, zwei SANs, widerrufene SPKI, `BEGIN_DIR` am
  Onion-Listener, `LISTEN_FDS = 2`.

---

## 17. Was dieser Plan **nicht** behauptet

- dass `comnet-arti` heute existiert oder bezogen werden kann (der Split und
  die Veröffentlichung sind comnet-Arbeit nach
  `comnet/docs/plans/arti-bootstrap-crate-contract.md`; bis dahin ist W3
  blockiert);
- dass Cloudflare „Onion Routing“ oder der Tor-Browser-Umgang mit
  `Onion-Location`/`Alt-Svc` in Zukunft unverändert bleibt (Stand der
  Recherche am 2026-11-01, Quellen in `01-recherche…`; Auto-Redirect ist
  bereits 2024 entfallen);
- dass `onion-restricted` vor `comnet-arti 0.2.0` (O12)
  funktioniert;
- dass `pq-strict` mit Browsern funktioniert (es ist für harw-Clients
  gedacht);
- dass der Rate-Limit-Schlüssel pro Onion-Stream einen Circuit-Angreifer
  bremst (bekannte Lücke C6, offen bis W0 Klarheit bringt).
