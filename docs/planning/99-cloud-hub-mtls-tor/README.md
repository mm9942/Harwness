# PL-99 — Cloud Hub über PQ-mTLS und Arti/Tor

> **Status:** Contract-Draft (Konzept, nichts davon umgesetzt)
> **Stand:** Code bei `9befeb19` (Branch `ccr-859b52e5-uecpa8`)
> **Ziel:** den Cloud-Aufbau über das bestehende PQ-mTLS-Knotennetz und neu über Tor
> (arti, eingebettet) ausbauen, absichern und durchgehend verdrahten. Zieltopologie:
> **zentraler Cloud Hub**.
> **Bedrohungsmodell:** [THREAT-MODEL.md](THREAT-MODEL.md)

Tor wird für zwei Dinge gebraucht:

1. **Erreichbarkeit:** Ein Knoten oder der Hub ist über einen Onion-Dienst erreichbar. Dafür
   braucht es keinen offenen Port, keine öffentliche IP und keinen Reverse-Proxy. Daten fließen als
   `Vec<u8>`-Nachrichten in Tor hinein und wieder heraus.
2. **Ausgehende Agent-Recherche:** Web-Werkzeuge der Agenten können über Tor laufen.

Quellen: PL-65 (`CLOUD-HOME-HUB-PROFILES-V2.md`, `CLOUD-HUB-WIRING.md`), PL-68, PL-89, PL-94,
`linux-user-cloud-control/04-cloud-control-contract.md` und `08-cloud-implementation-status.md`,
`SECURITY.md`, `harw-node-transport/README.md`. Für Tor ist die Vorlage das Projekt `mm9942/comnet`
(`crates/comnet-transport`, DEC-004/009/014/027, `docs/security/THREAT_MODEL.md`).

---

## 1. Befundene Fakten (bottom-up)

### 1.1 L0 – Schlüssel: `harw-auth-hub`

- Lokaler KMS über AF_UNIX mit SO_PEERCRED. Er hält ML-DSA-65-Knotenschlüssel unter
  `harw.node-identity/<node>` mit dem Zweck `NodeHandshake`. Der Prozess sieht nie den privaten Schlüssel.
- Kein CA, keine Zertifikate, keine Enrollment-Tokens.
- Der Keystore liegt **standardmäßig nur im Speicher** (`harw-auth-hub/src/config.rs:235-237`). Einen versiegelten
  Datei-Store gibt es (`sealed/mod.rs`). Nach einem Neustart des Hubs ändert sich der Knotenschlüssel. Der
  Selbsttest in `authhub_signer.rs:231-239` erkennt das, aber jeder Pin der Peers bricht.

### 1.2 L1 – Kanal: `harw-node-transport/src/tls.rs`

- TLS 1.3, Schlüsselaustausch nur `X25519MLKEM768` (`:44`), kein Downgrade. rustls 0.23 mit aws-lc-rs.
- Der Server nutzt einen Ed25519-Raw-Public-Key (RFC 7250) je `NodeTransportServer::new` (`:52`).
  Keine Tickets und keine Resumption (`:71`, `:88`), kein SNI (`:87`), ALPN `http/1.1`.
- Der Client akzeptiert jeden Server-Schlüssel (`ChannelOnlyVerifier`, `:83`, `:130`). Das ist gewollt: TLS
  ist nur der Kanal, die Identität kommt aus L2.
- Kanalbindung: `export_keying_material(32, "EXPORTER-harw-node-transport-v1")` (`:102`, `:109`).

### 1.3 L2 – Handshake: `harw-node-transport/src/handshake.rs`

- Ablauf: `ClientHello{v, client_id, server_id, Nc, Tc}` → `ServerHello{…, Sig_S}` → `ClientFinish{Sig_C}`
  → `Accept`.
- Das Transkript (`:288-307`) bindet Domain, Rolle, Version, beide IDs, Nonces, Zeitstempel und den Exporter.
- Replay-Cache mit 65 536 Einträgen, fail-closed (`:412`). Zeitfenster ±30 s (`:81`). Strenge Dekodierung,
  Frames höchstens 16 KiB, geprüft vor der Allokation (`wire.rs`).
- **Der Server signiert, bevor der Client bewiesen ist** (`accept_hello` `:551-588`, danach `sign()`;
  `server.rs:262-275`). Geprüft wird vorher nur, dass die Client-ID *bekannt* ist (`is_known`, `:569`).

### 1.4 L3 – Socket: `harw-node-transport`

- **Fest an TCP gebunden:**
  - `NodeTransportClient::connect_with` → `TcpStream::connect(addr)` (`client.rs:106`);
  - `serve_upgradable(listener: TcpListener, …)` (`upgrade.rs:131`);
  - `accept(tcp: TcpStream)` (`server.rs:209`).
- **Schon generisch:** `server_handshake<S>` und `client_handshake<S>` sowie die Hyper-Ebene. Ein Stream mit
  `AsyncRead + AsyncWrite + Unpin` genügt.
- Limits: 256 globale Slots (`server.rs:43`, `:185`), 10 s Handshake-Frist, 30 s `header_read_timeout`.
  **Kein** Limit je Quelle, **keine** Idle- oder Max-Lebensdauer nach der Authentisierung.
- Pins sind zur Laufzeit unveränderlich (`Arc<dyn NodeVerifier>`, `server.rs:51`). Eine Änderung verlangt
  einen Neustart (`session_serve_remote.rs:160`).

### 1.5 L4 – Listener: `harw-node-listener`

- Hat keinen eigenen Socket und bekommt einen `TcpListener` (`lib.rs:240`).
- Das Geräteregister `node-devices.conf` (`node|device|tenant|tier|status|label[|approve]`) wird
  bei jeder Zuordnung gelesen (`identity.rs:280`, `open_nofollow`).
- Live-Widerruf über `HostRevoker`/`LiveConnections` (`revoke.rs`), gepollt alle 5 s
  (`harw-cli/src/session_serve_remote.rs:427`).
- `DeviceRegistry::enroll` (`identity.rs:180`) hängt Zeilen ohne `nofollow` und mit der Standard-umask an.
  `records`/`mark_revoked` (`:193`, `:223`) folgen Symlinks.

### 1.6 L5 – Session

- `harw gateway` (`harw-cli/src/gateway.rs:489-511`) → `session_listener::start_from_config`
  (`session_listener.rs:237`) → `session_serve_remote::start_remote` (`:319`, `:397`) → `NodeListener`
  → `harw-session-ws` → `SessionHost`.
- `harw attach --host ALIAS` (`attach_cmd.rs:127`) liest den Alias aus `$HARW_STATE_DIR/hosts/<alias>.json`.
  Diese Datei wird von Hand geschrieben, `AliasStore::put` hat keinen CLI-Aufrufer. Die Client-Pins werden
  locker gelesen (`attach_cmd.rs:216-218`: folgt Symlinks, unbegrenzt, ein Duplikat überschreibt still).
  `lookup_host` (`:233`) löst DNS im Klartext auf.
- Reconnect (`BackoffPolicy`, `ResumeCursors` in `harw-session-remote/src/reconnect.rs`) ist ungenutzt.
  Der Remote-SessionHost ist vom lokalen getrennt.

### 1.7 L6 – Cloud

| Komponente | Zustand | Beleg |
|---|---|---|
| `harw-cloud-gateway::proxy` (Feature `proxy`) | Platzhalter, beantwortet jede Anfrage mit HTTP 501 | `proxy.rs:19-28` |
| `harw-cloud-enroll::validate_pairing_at` | prüft nur Code und Ablauf, hat keinen Aufrufer | `lib.rs:99` |
| `cloud.cloudctl.enroll/revoke` | lehnt immer ab | `harw-cloud-ops/src/cloudctl.rs:44-46`, `104-121` |
| `harw-cloudctl enroll/revoke` | lehnt ab | `harw-cloudctl/src/main.rs:160-165` |
| `cloud.status` | installed/configured/running/ready immer `null` | `harw-cloud-ops/src/status.rs:291` |
| `gateway.listeners` | Tabelle wird nie gefüllt | `harw-session-host/src/host.rs:455` |
| `harw node init`/`doctor` | fehlt (Zyklus N2) | `harw-cli/src/node_cmd.rs:7`, `:387` |

### 1.8 Egress und Netz-Policy

- `harw-authority`: `EgressTarget::{Host, DnsSuffix, Cidr, PublicDns}` (`lib.rs:311`). `onion` steht in
  `NON_PUBLIC_SUFFIXES` (`lib.rs:345`), deshalb lässt `PublicDns` `.onion` nie zu.
- `harw-egress` löst Namen per `lookup_host` auf (`client.rs:66`, `proxy.rs:361`). Ein expliziter
  Allowlist-Eintrag `Host("x.onion")` würde **DNS leaken**.
- `harw-tailscale` (`harw web --tailnet`) leitet Klartext-HTTP an harw-web weiter und lässt jeden Tailnet-Login
  als Operator zu (`gate.rs:9-11`). Die Knoten-Identität wird dabei umgangen.

### 1.9 Tor

Im Repo gibt es keinen Bezug zu Tor, arti oder Onion-Diensten. Die einzige Stelle ist die oben genannte Sperre
für `.onion`.

### 1.10 Vorlage comnet (`mm9942/comnet`, `crates/comnet-transport`)

- Traits `Conn`/`BoxConn` (`conn.rs:40-45`), `Dialer` (`dialer.rs:84`), `Listener` (`listener.rs:52`) und
  `Target{Tcp,Unix,Onion,Duplex}` (`dialer.rs:42`).
- arti: `ArtiRuntime::bootstrap`, `ArtiDialer` (nur `.onion`), `OnionListener`
  (`launch_onion_on_port`; nur `Begin` auf dem Port, alles andere `End(DONE)`) (`arti.rs:87-405`).
- Isolation pro Principal × Domäne (`isolation.rs`), `TransportPolicy{require_anonymity}` fail-closed
  (`policy.rs`), `DuplexHub` für Offline-Tests.
- **Keine** `Vec<u8>`-Nachrichten-API über Tor; alle Transporte liefern rohe Byte-Streams. Das Muster für
  Längenrahmen gibt es dort (`comnet-vault/src/ipc.rs:536-569`) und bei uns (`harw-node-transport/src/wire.rs`).
- Nicht konfiguriert: Restricted Discovery, hs-PoW, Dial-Timeouts. Nie kompiliert (kein `target/`).

---

## 2. Leitplanken (verbindlich)

Aus `local-own-cloud-websocket.md` §11, PL-65 §4 / DEC-018, `harw-node-transport/README.md`, `SECURITY.md`:

1. **Tor ist nur Underlay.** PQ-TLS + gegenseitiges ML-DSA laufen immer *innen*, nie wird ein Teil ersetzt.
2. Eine Adresse (IP, Hostname, Onion) ist **nie** Identität. Identität kommt nur aus `AuthenticatedPeer`.
3. Unbekannte Peers werden abgelehnt, **bevor** der Server signiert (nach T0 auch: bevor er überhaupt etwas Teures tut).
4. Rechte verengen sich nur. Ein Remote-Tenant ist immer `Some`. Jede Methode autorisiert ihre Zielsession selbst.
5. Kein loopback-TCP ohne Authentisierung. Ein Onion-Ingress endet **im Prozess**, nicht an einem lokalen Port.
6. Kein Weg vorbei an `OperationRegistry` oder der Freigabe-Policy. Remote-Freigabe ist ein Opt-in pro Gerät.
7. Ein Widerruf beendet auch Live-Verbindungen.
8. Keine Provider-Credentials, KEKs oder Seeds an Clients.
9. **Kein stiller Fallback:** Ein fehlgeschlagener Tor-Weg wird nie direkt versucht. Ein gescheitertes Cloud-Attach
   erweitert nie lokale Autorität.
10. Neue Abhängigkeiten nur in Ring A. Kein J- oder TCB-Crate erreicht arti (`arch-policy.toml`). `deny.toml`-Review,
    `unsafe_code = "forbid"`.
11. Ein Child-Netzscope ist nie breiter als der des Elternteils. Read-only-Rollen bekommen nie Netz. Ein nicht
    vertrautes Projekt darf nur verengen.

---

## 3. Zielarchitektur

```
Gerät (TUI/CLI/Mobile/SDK)
  └─ NodeDialer { Tcp | Host | Onion | Duplex }  ── isoliert je (Gerät × Ziel)
        ▼   PQ-TLS 1.3 (X25519MLKEM768) + ML-DSA-65-Handshake v2 (Client zuerst) — unverändert innen
Cloud Hub  (Profil CloudHub: Verzeichnis · Routing · Policy · Pairing · Widerruf; keine Ausführung)
  Ingress: TCP (privat/tailnet) und/oder Onion (arti in-process, Restricted Discovery, optional PoW)
  ├─ NodeChannel (Vec<u8>-Frames) ⇄ Uplinks der Knoten
  └─ Verzeichnis: SessionId → (node_id, placement_generation)
Knoten (SessionHost/Worker): wählt sich AUSGEHEND zum Hub (TCP oder Onion) → kein offener Port beim Knoten
Agenten-Egress: harw-egress → TorConnector (Isolation je Agent-Lauf × Ziel), require_anonymity fail-closed
```

Der Hub ist ein normaler Knoten mit Verzeichnisrolle. Er leitet Session-Frames weiter, autorisiert vorher und
gibt eine signierte Attestation des Endgeräts mit. Der Knoten autorisiert jeden Zugriff **noch einmal**, der Hub
verstärkt keine Rechte.

---

## 4. Phasen (je ein PR, Reihenfolge verbindlich)

### T0 – Härtung der Transportbasis

Voraussetzung für alles mit Tor.

- **Handshake v2, Client zuerst.** `ClientHello` trägt eine ML-DSA-Signatur des Clients über
  (Domain ‖ Client-ID ‖ Server-ID ‖ Nc ‖ Tc ‖ Exporter). Der Server prüft sie **vor** jedem `sign()`.
  Danach folgen `ServerHello` und `ClientFinish` wie bisher, das Transkript bleibt vollständig gebunden. v1 bleibt
  per `HandshakePolicy::min_version` abschaltbar (Entscheidung D1). Dateien: `handshake.rs`, `server.rs`,
  `client.rs`, `src/tests.rs`.
- **Limits:**
  - Token-Bucket je Quelle (IP bzw. Onion-Stream-Kontext);
  - getrennte Pre-Auth- und Post-Auth-Slots;
  - `idle_timeout` und `max_lifetime` in `ServerOptions` (`server.rs:43`).
- **Veränderliche Pins:**
  - `NodeVerifier` hinter `ArcSwap` mit `reload()` aus `node-peers.conf`.
  - Entpinnen trennt Live-Verbindungen (Brücke zu `HostRevoker`).
  - Rotation: zwei Schlüssel je ID mit `not_after`.
- **Strenges Dateilesen überall:** `read_peer_pins` (`session_serve_remote.rs:163`) wird auch für
  `attach_cmd.rs:216` und für das `DeviceRegistry` verwendet (`open_nofollow`, 0600, begrenzt).
- **AuthHub:** Der versiegelte Datei-Keystore wird Standard für Knoten-Identitäten.
- **Tests:**
  - Negativtests für TLS 1.2 und Clients ohne PQ (Muster comnet `tests/tls_direct.rs`);
  - Slot-Erschöpfung und Slow-Loris;
  - Orakel: Ein unsigniertes oder falsch signiertes `ClientHello` löst **keine** AuthHub-Signatur aus.

### T1 – Transport-Abstraktion und `Vec<u8>`-Kanal

- Neu `harw-node-transport/src/io.rs` (Vorlage comnet `conn.rs`, `dialer.rs`, `listener.rs`):
  ```rust
  pub trait Conn: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
  pub type BoxConn = Box<dyn Conn>;
  pub trait NodeDialer: Send + Sync + 'static {
      fn dial(&self, to: NodeLocator, isolation: Isolation) -> BoxFuture<'_, Result<BoxConn, TransportError>>;
  }
  pub trait NodeAcceptor: Send + 'static {
      fn accept(&mut self) -> BoxFuture<'_, Result<(BoxConn, PeerContext), TransportError>>;
  }
  #[non_exhaustive]
  pub enum NodeLocator { Tcp(SocketAddr), Host { host: String, port: u16 }, Onion { addr: String, port: u16 }, Duplex(String) }
  ```
- `PeerContext { kind, remote: Option<SocketAddr> }`. Bei Onion gibt es keine Adresse, und sie ist nie Identität.
- `NodeEndpoint.addr: SocketAddr` (`harw-session-remote/src/conn.rs:79`) wird `NodeLocator`.
- `connect_with` / `serve_upgradable` / `accept` werden generisch über `BoxConn`. Die TCP-Varianten bleiben als
  dünne Wrapper, es gibt keinen API-Bruch. Timeouts gelten je Transport (Onion 60–120 s).
- `DuplexHub` (in-memory) für Offline-Tests (Vorlage comnet `duplex.rs`).
- **`Vec<u8>`-Nachrichtenkanal** `harw-node-transport/src/message.rs`:
  ```rust
  impl NodeChannel {
      pub async fn send(&mut self, msg: &[u8]) -> Result<(), TransportError>;
      pub async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError>;
  }
  ```
  - Er läuft über den authentisierten Stream (nach Handshake v2) und nutzt `wire.rs` wieder: u32-BE-Länge,
    leere Frames werden abgelehnt, die Obergrenze (Standard 1 MiB) wird vor der Allokation geprüft.
  - Er ist transportunabhängig und damit der Weg **in Tor und aus Tor raus ohne Reverse-Proxy**. Hub-Uplink (T5)
    und alle Knoten-zu-Knoten-Nachrichten nutzen ihn.

### T2 – Neues Crate `harw-tor`

Port von `comnet-transport`, Feature `arti`, nur in Ring A.

- **Abhängigkeiten** exakt gepinnt (comnet DEC-004), monatlicher Bump als eigener PR:
  ```toml
  arti-client   = { version = "=0.47.0", default-features = false, features = ["tokio","rustls","onion-service-client","onion-service-service","compression","static-sqlite"] }
  tor-hsservice = { version = "=0.47.0", default-features = false }
  tor-rtcompat  = { version = "=0.47.0", default-features = false, features = ["tokio","rustls"] }
  tor-cell      = { version = "=0.47.0", default-features = false }
  tor-proto     = { version = "=0.47.0", default-features = false, features = ["tokio"] }
  safelog       = "=0.10.0"
  ```
  Nie das Feature `full`. Nie native-tls oder openssl.
- **Laufzeit:**
  - `ArtiRuntime::bootstrap(TorConfig{state_dir, cache_dir, memquota})`.
  - `ArtiDialer` wählt nur `.onion` für Knotenverkehr an.
  - `OnionAcceptor` (`launch_onion(nickname, port)`) implementiert `NodeAcceptor`.
  - Vorlage: comnet `arti.rs:87-405`.
- **Isolation** `Isolation::for_domain(principal, purpose)` (SHA-256, längenpräfixiert) als `CircuitIsolation`.
  `fresh()` für Einmalnutzung. Vorlage: comnet `isolation.rs`.
- **`TransportPolicy{require_anonymity}` + `PolicyDialer`**, fail-closed bei der Konstruktion und bei jedem Dial.
  Vorlage: comnet `policy.rs`.
- **Neu gegenüber comnet:**
  - Restricted Discovery (Onion-Client-Autorisierung): Nur gepairte Geräte können den Deskriptor überhaupt lesen.
  - hs-PoW, abschaltbar.
  - Dial- und Bootstrap-Timeouts.
  - Status `idle|bootstrapping|ready|failed` (Muster comnet `LazyArtiDialer`).
- **Provider (comnet DEC-009):** Die Prozess-Vorgabe ist „PQ zuerst mit klassischem Fallback“, nur für Tor-Links,
  sonst bootstrappt arti nicht. `harw-node-transport` übergibt weiter **explizit** seinen PQ-only-Provider an die
  eigenen Configs.
- **Zustand** unter `<profile>/tor/{state,cache}` mit 0700, `SQLITE_TMPDIR` im Cache. systemd:
  `StateDirectoryMode=0700`, `UMask=0077`.
- **Lieferkette:**
  - `deny.toml`: Lizenzausnahmen `xxhash-rust` (BSL-1.0) und `async_executors` (Unlicense), begründet.
  - Gates `cargo deny check` und `cargo tree -i ring --features arti`.
  - Arch-Gate: kein J- oder TCB-Crate erreicht `harw-tor`.

### T3 – Erreichbarkeit verdrahten

- Konfiguration:
  ```toml
  [session_listener]
  transports = ["tcp", "onion"]

  [tor]
  nickname = "harw-node"
  port = 443
  restricted_discovery = true
  pow = false
  require_anonymity = false
  ```
  Dateien: `harw-config/src/session_listener_toml.rs`, `harw-cli/src/session_listener.rs`.
- `start_remote` nimmt mehrere Acceptors (TCP + Onion) auf **denselben** `NodeListener`. Dafür wird
  `harw-node-listener/src/lib.rs:240` generisch. Es gibt keinen loopback-TCP-Hop (Leitplanke 5).
- `harw attach --host` mit `NodeLocator::Onion`: Arti-Dialer mit Isolation je (Gerät, Ziel-Knoten). Ohne das Feature
  `arti` oder ohne Bootstrap gibt es einen klaren Fehler, nie einen Fallback auf direkt (Leitplanke 9).
- `ListenerKind::Onion` (`harw-protocol/src/session_wire.rs`). `gateway.listeners` meldet die echten Listener
  (`register_listener` bekommt einen Produktionsaufrufer).

### T4 – Pairing und Enrollment

- **Pairing-String** (auch als QR):
  `harw-pair:v1:<hub-node-id>:<ml-dsa-fingerprint>:<locator(onion|host)>:<128-bit-secret>`.
  Er pinnt den Fingerprint und die Erreichbarkeit zugleich.
- **Ablauf** (PL-94 §3.2, PL-65 DEC-014):
  1. Der Host erzeugt ein Einmal-Secret. Gespeichert ist nur der Digest, mit TTL, Rate-Limit und Burn nach Fehlversuchen.
  2. Das Gerät verbindet sich über einen **separaten Enrollment-Acceptor**. Dort ist nur der Server authentisiert.
  3. Das Gerät schickt seinen ML-DSA-Public-Key und HMAC(secret, Transkript).
  4. Zustände: Issued → Presented → **Approved** (Operator bestätigt) → Committed.
  5. Pins und Register werden atomar geschrieben. Bei Onion wird der Restricted-Discovery-Client-Key des Geräts
     aufgenommen. Das Tier legt der Host fest, nie das Gerät.
- **Verdrahtung:**
  - `harw-cloud-enroll::validate_pairing_at`, `DeviceRegistry::enroll`;
  - `cloud.cloudctl.enroll/revoke`, `harw-cloudctl enroll/revoke`;
  - CLI `harw device pair|join|approve` und `harw node init` (N2);
  - der Alias wird beim Join automatisch geschrieben (`AliasStore::put`).

### T5 – Zentraler Cloud Hub

- **Profil `CloudHub`** (PL-65 V2 §4): nur Verzeichnis, Routing, Policy und Pairing. Keine Shell, kein
  Workspace-Schreibzugriff, kein Container-Socket, nie im selben Prozess wie ein Ausführungs-Hub.
- **Uplink-Protokoll** über `NodeChannel`:
  - Ein Knoten wählt sich ausgehend zum Hub (TCP oder Onion) und registriert
    `NodeHello{node_id, sessions[], capabilities, placement_generation}`, mit Heartbeat.
  - Der Lebenszyklus kommt aus `harw-netsec` (Pending → Active → Draining → Drained / Revoked). Gespeist wird er
    jetzt über den Uplink, das schließt die netsec-Lücke „no heartbeats“.
- **Verzeichnis:** `SessionId → (node_id, placement_generation)` dauerhaft im Hub-State. Die Generation ist das
  Fencing-Token. In v1 gibt es kein Failover und keine Live-Migration.
- **Routing:**
  1. Ein Gerät verbindet sich mit dem Hub.
  2. Der Hub autorisiert (Gerät × Session × Tier).
  3. Er leitet die `harw.session.v1`-Frames als `Vec<u8>` über den Uplink weiter und legt eine signierte
     Hub-Attestation `(device, tenant, tier, session, exp)` bei.
  4. Der Knoten prüft die Attestation und autorisiert erneut (Entscheidung D2).
- `harw-cloud-gateway` ersetzt den 501-Platzhalter durch diesen Router. Gebaut wird er auf node-transport bzw. hyper;
  Pingora nur an der Browser-Kante, nie zwischen Knoten (PL-89).
- Ein Reconnect-Supervisor nutzt `BackoffPolicy` und `ResumeCursors` (`harw-session-remote/src/reconnect.rs`).

### T6 – Agenten-Egress über Tor

- `EgressTarget::Onion(addr)` (`harw-authority/src/lib.rs`) und die Netzpolicy
  `[network].tor = "off" | "onion_only" | "all"` (`harw-config`), Vorgabe `off`. Ein Child-Scope ist nie breiter
  als der des Elternteils.
- `harw-egress`: `.onion` wird **nie** per DNS aufgelöst. Das behebt das Leak in `client.rs:66` / `proxy.rs:361`.
  Für Tor-Ziele gibt es einen hyper-Client mit `ArtiDialer`-Connector statt reqwest.
- Isolation je (Agent-Lauf × Ziel-Domain). `require_anonymity` je Rolle, zum Beispiel `researcher-web` mit
  `tor = "all"`: Dann gibt es keinen Direktpfad.
- Exit-Verkehr nur bei `all` (Entscheidung D4). Allowlist und Freigabe gelten weiter. Metadaten-, private und
  CGNAT-Ziele bleiben verboten.

### T7 – Betrieb und Beobachtbarkeit

- `cloud.status` liefert echte Werte: Listener, Transporte, Tor-Bootstrap, Uplinks, Pins, ablaufende Pairings.
  `harw-cloudctl status` liest die echten Listener statt Ports zu proben.
- systemd-Units für Hub und Gateway (`ProtectSystem=strict`, `StateDirectoryMode=0700`, `UMask=0077`).
  Das Podman-Quadlet `packaging/podman/quadlet/harw-control.container` wird repariert.
- Logs über `safelog` mit Redaction: Onion-Adressen erscheinen nur gekürzt, IDs gehasht. Zähler je
  Ablehnungsgrund.

---

## 5. Offene Entscheidungen

| ID | Frage | Empfehlung |
|---|---|---|
| D1 | Handshake v2: mit Übergangsfenster (v1 abschaltbar) oder sofort strikt? | Übergangsfenster, v1 standardmäßig aus |
| D2 | Leitet der Hub Frames nur weiter oder terminiert er Sessions? | weiterleiten + erneute Autorisierung am Knoten |
| D3 | Onion- und TCP-Ingress gleichzeitig oder `require_anonymity` je Listener? | je Listener konfigurierbar |
| D4 | Exit-Verkehr für Agenten (`tor = "all"`) oder nur Onion-Ziele? | zuerst `onion_only`, `all` nur per Rolle |
| D5 | Arti-Versionspflege | exakter Pin, monatlicher Bump als eigener PR |
| D6 | Gemeinsamer SessionHost für lokal und remote | nach T5 entscheiden |

## 6. Wiederverwendung

- **Harwness:**
  - `wire.rs` (Längenrahmen), `read_peer_pins` (strenges Lesen), `HostRevoker`/`LiveConnections`;
  - `harw-netsec` (Lebenszyklus), `BackoffPolicy`/`ResumeCursors`;
  - `harw-cloud-enroll`, `AliasStore`, `AuthHubNodeSigner`/`HubSign`.
- **comnet:**
  - `comnet-transport` (`conn.rs`, `dialer.rs`, `listener.rs`, `arti.rs`, `isolation.rs`, `policy.rs`, `duplex.rs`);
  - Tests (`tests/policy.rs`, `tests/local_transport.rs`, `tests/tls_direct.rs`, `tests/arti_smoke.rs` als `#[ignore]`);
  - `deploy/systemd/comnet-apicon.service` (Härtung), DEC-004/009/014.

## 7. Tests und Abnahme

- **Offline (CI), über `DuplexHub`:**
  - Handshake v2 und Orakel-Test, Slots und Slow-Loris;
  - Neuladen und Widerruf von Pins;
  - `NodeChannel`: Roundtrip, leere und zu große Frames;
  - `PolicyDialer` fail-closed (Mock-Dialer);
  - Pairing-Zustandsautomat;
  - Hub-Routing mit zwei Knoten;
  - Egress: `.onion` wird nie per DNS aufgelöst (Resolver-Spy).
- **Gates:** `cargo fmt --check`, `cargo clippy -D warnings`, `cargo deny check`, Arch-Gate,
  `cargo tree -i ring --features arti`.
- **Live (manuell, `#[ignore]`):**
  - Arti-Bootstrap, Onion-Launch;
  - `harw attach` über Onion mit Restricted Discovery;
  - ein Hub mit dem Uplink eines Knotens hinter NAT.
