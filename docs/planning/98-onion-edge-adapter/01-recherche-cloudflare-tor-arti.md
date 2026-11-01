---
id: PL-98-RECHERCHE
title: "Recherche: Cloudflare ↔ Tor, Tor Browser, Zertifikate für .onion, arti 0.47 / 2.7.0, systemd-Prüfung, Crate-Stände, MagicDNS"
status: Befunde mit Quellen (Stand 2026-11-01)
date: 2026-11-01
parent: README.md
---

# Recherche zu PL-98 (Stand 2026-11-01)

Jede Aussage trägt ihre Quelle. Was nicht belegt werden konnte, steht unter
„Nicht verifiziert“. Belege aus den Repos (comnet, Harwness) stehen im
[Plan](README.md) §2; hier geht es um Externes.

## F1. Cloudflare und Tor

| # | Befund | Quelle |
|---|---|---|
| F1.1 | **Cloudflare „Onion Routing“** (Zonen-Einstellung `opportunistic_onion`, alle Tarife, Doku zuletzt 2026-08-14, keine Abkündigung) liefert Tor-Browser-Nutzern beim ersten Request einen `Alt-Svc`-Header auf eine von **Cloudflares eigenen** `.onion`-Adressen; der Tor Browser baut dann einen Circuit zu Cloudflares Onion. Die sichtbare Domain bleibt die Clearnet-Domain. Cloudflare stellt **kein** Zertifikat für die `.onion` aus; die Onion ist nicht direkt per HTTPS aufrufbar. | https://developers.cloudflare.com/network/onion-routing/ |
| F1.2 | Mechanik (Cloudflare-Blog 2018): `alt-svc: h2="cflare…onion:443"; ma=86400; persist=1`; zehn Cloudflare-Onions; der Onion-Service reicht den TLS-Handshake an Cloudflares Edge weiter, die das Zertifikat der Clearnet-Domain präsentiert. „Opportunistic Onions do not fully protect against attackers who can simply remove the alternative service header.“ | https://blog.cloudflare.com/cloudflare-onion-service/ |
| F1.3 | Der **Origin muss für Cloudflare normal erreichbar** sein (Zwei-Verbindungs-Modell: Besucher ↔ Cloudflare, Cloudflare ↔ Origin). Onion Routing wirkt nur auf **proxied** (orange) Records; DNS-only (grey) Records werden nicht durch Cloudflare geroutet und bekommen weder Onion Routing, WAF noch Transform Rules. | https://developers.cloudflare.com/ssl/origin-configuration/ssl-modes/ ; https://developers.cloudflare.com/dns/proxy-status/ |
| F1.4 | Der Tor Browser nutzt Cloudflares Onion-`Alt-Svc` weiterhin (aktive Tickets 2025–2026 über wechselnde Cloudflare-Onions und Performance). | https://gitlab.torproject.org/tpo/applications/tor-browser/-/issues/43847 ; https://gitlab.torproject.org/tpo/applications/tor-browser/-/issues/44248 |
| F1.5 | **`Onion-Location`** (Tor-Browser-Spec-Proposal 100, Status *Finished*): Wert muss eine `http:`/`https:`-URL mit `.onion`-Host sein; die Seite muss **über HTTPS** ausgeliefert werden und darf selbst keine Onion sein. Meta-Variante `<meta http-equiv="onion-location" content="…">`. | https://gitlab.torproject.org/tpo/applications/tor-browser-spec/-/raw/main/proposals/100-onion-location-header.txt ; https://community.torproject.org/onion-services/advanced/onion-location/ |
| F1.6 | **Keine automatische Umleitung mehr:** Die Option „.onion-Seiten bevorzugen“ (Auto-Redirect) wurde in Tor Browser **13.0.12 (2024-03-19)** wegen eines Fingerprinting-Risikos entfernt. Heute zeigt der Browser eine „.onion available“-Pille in der Adressleiste; der Nutzer klickt. | https://blog.torproject.org/new-release-tor-browser-13012/ ; https://support.torproject.org/onion-services/ |
| F1.7 | Cloudflare **Response Header Transform Rules** können beliebige Antwort-Header setzen, auch mit Ausdrücken aus dem Request (z. B. `concat("http://<addr>.onion", http.request.uri.path)`); verboten sind nur `cf-`/`x-cf-`-Präfixe und wenige Namen wie `server`. `Onion-Location` ist erlaubt. Gilt nur für proxied Zonen (F1.3). | https://developers.cloudflare.com/rules/transform/response-header-modification/ |
| F1.8 | **`Alt-Svc`** (RFC 7838): `alt-authority = "[uri-host] ":" port"`, Parameter `ma`, `persist`; ersetzt den Origin nicht; der Client braucht „reasonable assurances“, dass der alternative Dienst für den Origin gültig ist, d. h. das **Zertifikat muss für den Clearnet-Host gelten**. Deshalb kann Cloudflare das (Edge hält das Zonen-Zertifikat). Ein selbst gehosteter `Alt-Svc` auf die eigene Onion funktioniert nur, wenn der Onion-Stream dasselbe, für die Domain gültige Zertifikat präsentiert. Cloudflare bietet **kein** Feature, `Alt-Svc` auf eine **Kunden**-Onion zu setzen. | https://www.rfc-editor.org/rfc/rfc7838.html ; https://blog.cloudflare.com/cloudflare-onion-service/ |
| F1.9 | **DNS kann nicht auf `.onion` zeigen** (RFC 7686): Resolver antworten mit NXDOMAIN oder lösen über Tor; autoritative Server **müssen** NXDOMAIN liefern; Registrare dürfen `.onion` nicht registrieren. Ein CNAME/SRV/HTTPS-Ziel `x.onion` ist für jeden konformen Resolver unauflösbar. | https://www.rfc-editor.org/rfc/rfc7686.html |
| F1.10 | **Kein TXT-Standard** für Onion-Adressen. Der „Onion Plan“ des Tor-Projekts (Draft 2024.Q3) wägt TXT (RFC 1464), SRV, HTTPS/SVCB (RFC 9460) und einen neuen `ONION`-RR ab, definiert aber keine Syntax und verlangt: „DNS-based address discovery MUST be OPTIONAL“. Ein Drittprojekt (Namecoin/prop279) legt die Onion in einen TXT unter `_tor.<domain>`. | https://onionservices.torproject.org/research/appendixes/dns/ ; https://bag-pr.duckdns.org/2017/06/21/tor-prop279.html |
| F1.11 | **Cloudflare Tunnel (`cloudflared`)** ist ein Cloudflare-Produkt (ausgehende Verbindung vom Origin zu Cloudflare), **kein Tor**, erzeugt keine `.onion`. Authenticated Origin Pulls ist mit Tunnel inkompatibel. | https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/ ; https://developers.cloudflare.com/ssl/origin-configuration/authenticated-origin-pull/ |
| F1.12 | **Zertifikate für `.onion`:** CA/B-Forum Baseline Requirements (v2.3.1, 2026-10-04), Anhang B: v3-Onion-Adressen, Kontrolle per ACME `http-01`/`tls-alpn` **über Tor** oder per CSR mit `caSigningNonce`/`applicantSigningNonce`, der mit dem HS-Schlüssel signiert ist; DNS-Methoden und CAA ausgeschlossen; Wildcards erlaubt (B.2.b). DV/OV seit Ballot SC27v3 (BR 1.6.9, wirksam 2020-03-27). **HARICA** stellt DV für v3-Onions aus (seit 2021-04-26). **Let's Encrypt** stellt **keine** `.onion`-Zertifikate aus (Stand 2024-07-30 „no plans … in the near future“, nichts Neueres gefunden). | https://github.com/cabforum/servercert/blob/main/docs/BR.md ; https://news.harica.gr/tags/onion/ ; https://blog.torproject.org/tls-certificate-for-onion-site/ ; https://community.letsencrypt.org/t/when-will-lets-encrypt-let-onion-owners-encrypt/222577 |
| F1.13 | **Sauteed Onions** (WPES ’22): Domain↔Onion-Bindung über TLS-Zertifikate, sichtbar in Certificate-Transparency-Logs; Tor-Proposal **361 „OASIC“** (offen, 2025-04-14) standardisiert SAN-Formate wie `[onion-address]onion[0-4].meaningfulname`; „There are no current implementations“. | https://www.freehaven.net/anonbib/cache/sauteedonions2022.pdf ; https://spec.torproject.org/proposals/361-oasic.html |
| F1.14 | **mTLS durch die orange Wolke ist nicht Ende-zu-Ende:** Cloudflare terminiert das Besucher-TLS; API-Shield-mTLS prüft Client-Zertifikate bei Cloudflare und reicht sie dem Origin nur als **Header** weiter (`Client-Cert` nach RFC 9440 oder `Cf-Client-Cert-*`), die „regardless of the certificate validation result“ gesetzt werden. Authenticated Origin Pulls ist die Gegenrichtung (Cloudflare zeigt dem Origin ein Zertifikat). Für echtes mTLS Nutzer→Dienst bleibt nur **DNS-only (grey cloud)**. | https://developers.cloudflare.com/ssl/client-certificates/forward-a-client-certificate/ ; https://developers.cloudflare.com/api-shield/security/mtls/configure/ ; https://developers.cloudflare.com/dns/proxy-status/ |

**Folgerung für den Plan:** Profil `domain` = grey cloud + eigene Header
(`Onion-Location` als Angebot, nicht als Umleitung) + signierter
`.well-known`-Datensatz + TXT-Konvention (harw-eigen, als optional
dokumentiert). `Alt-Svc` auf die eigene Onion ist nur sinnvoll, wenn der
Onion-Stream ein **öffentlich vertrauenswürdiges** Zertifikat der Domain
präsentiert (Let's Encrypt für die Clearnet-Domain, DNS-01 über Cloudflare,
grey cloud reicht) und `client_auth.mode != "required"`. Profil
`domain-orange` (Cloudflare-Proxy + Onion Routing + Transform-Rule-
`Onion-Location` auf die **eigene** Onion) nur für öffentliche Inhalte ohne
mTLS.

## F2. arti (Stand 2026-11-01)

| # | Befund | Quelle |
|---|---|---|
| F2.1 | **Versionskorrektur:** `arti-client 0.47.0` (und `tor-hsservice`, `tor-rtcompat` 0.47.0) wurde am **2026-10-01** veröffentlicht und entspricht **Arti 2.7.0**. Es ist die **aktuelle** Version; es gibt kein 0.48+. (Oktober 2025 waren 0.35.0/0.36.0 = Arti 1.6.0/1.7.0.) comnets Pin `=0.47.0` ist also aktuell. | https://crates.io/api/v1/crates/arti-client/versions ; https://gitlab.torproject.org/tpo/core/arti/-/raw/main/CHANGELOG.md |
| F2.2 | **MSRV 1.92** für arti 2.7.0 / arti-client 0.47.0 („MSRV has been increased to 1.92“). | CHANGELOG 2.7.0 |
| F2.3 | Feature `onion-service-service` aktiviert `keymgr`, `tor-hscrypto`, `tor-hsservice`, `tor-persist/state-dir`. `rustls` ist **nicht additiv** (Default `tokio` + `native-tls`). In `arti-client` sind `restricted-discovery` und `hs-pow-full` als **experimentell** gelistet. 2.7.0 entfernte `ctor-keystore` aus arti-client und deprecated `keymgr` als eigenes Feature. | https://docs.rs/crate/arti-client/latest/features ; CHANGELOG 2.7.0 |
| F2.4 | API 0.47.0: `TorClient::launch_onion_service(&self, config) -> Result<Option<(Arc<RunningOnionService>, impl Stream<Item = RendRequest>)>>`; `Ok(None)` bei deaktiviertem Dienst (Breaking Change seit Arti 1.8.0, 2025-12-01; comnet behandelt das, `arti.rs:235-240`). Zusätzlich `launch_onion_service_with_hsid(config, HsIdKeypair)` zum **programmatischen Import** eines Identity-Keys (schlägt fehl statt zu überschreiben). Offenes Ticket #1987: die beiden Funktionen liefern inkompatible `impl Stream`-Typen. | https://docs.rs/arti-client/latest/arti_client/struct.TorClient.html ; https://gitlab.torproject.org/tpo/core/arti/-/issues/1987 |
| F2.5 | `tor-hsservice` 0.47.0 `OnionServiceConfigBuilder`: `nickname`, `enabled`, `num_intro_points` (Default 3, max 20), **`rate_limit_at_intro = [rate, burst]`** (als DOS_PARAMS-Extension an Intro-Points, nicht lokal erzwungen; ≙ C-Tor `HiddenServiceEnableIntroDoS*`), **`max_concurrent_streams_per_circuit`** (≙ `HiddenServiceMaxStreamsCloseCircuit`), `enable_pow`, `pow_rend_queue_depth`, `restricted_discovery()`, `disable_pow_compilation`. `handle_rend_requests(RendRequest-Stream) -> StreamRequest-Stream`. | https://docs.rs/tor-hsservice/latest/tor_hsservice/config/struct.OnionServiceConfigBuilder.html ; https://docs.rs/tor-hsservice/latest/tor_hsservice/ |
| F2.6 | **Restricted Discovery** (Service-Seite) ist in `tor-hsservice` **nicht** experimentell (CHANGELOG 1.7.0, 2025-10-30: „no longer experimental“). Konfiguration: `[onion_services."nick".restricted_discovery] enabled = true`, dann `key_dirs = [{ path = ".../authorized_clients" }]` (Dateien `<name>.auth`) und/oder `static_keys = { alice = "descriptor:x25519:<base32>" }`; mindestens eines von beiden; `watch_configuration(bool)`. | https://docs.rs/crate/tor-hsservice/latest/features ; https://gitlab.torproject.org/tpo/core/arti/-/raw/main/crates/arti/src/arti-example-config.toml ; https://docs.rs/tor-hsservice/latest/tor_hsservice/config/restricted_discovery/struct.RestrictedDiscoveryConfigBuilder.html |
| F2.7 | **PoW** (`hs-pow-full`, Proposal 362) ist **experimentell** (seit Arti 1.5.0, 2025-08-28); 2.7.0 fixte TROVE-2026-068 (Panic in der PoW-Steuerschleife); offene Bugs #2800, #2675. `enable_pow = false` ist Default. | CHANGELOG ; https://gitlab.torproject.org/tpo/core/arti/-/issues/2800 |
| F2.8 | **Single Onion Services** (nicht-anonym, kürzere Circuits) werden **nicht** unterstützt (Ticket #727 offen). Jede harw-Onion ist damit standortverborgen, mit entsprechender Latenz. | https://gitlab.torproject.org/tpo/core/arti/-/issues/727 |
| F2.9 | **Vanguards:** `[vanguards] mode = "auto"|"full"|"lite"|"disabled"`; „auto“ = lite, wenn Feature `vanguards` aktiv (Default-Feature des `arti`-Binaries). Keine Features `vanguards-full`/`-lite`. Ob `arti-client` ohne das Feature baut, ist im Spike zu prüfen. | https://docs.rs/tor-guardmgr/latest/tor_guardmgr/enum.VanguardMode.html ; https://docs.rs/crate/arti/latest/features |
| F2.10 | **CLI/Keystore:** `arti hss --nickname N onion-address`; `arti hss ctor-migrate` (experimentell, Feature `onion-service-cli-extra`) migriert C-Tor-HS-Keys; `arti keys list|list-keystores|check-integrity`. Keystore-Pfade `hss/<nickname>/ks_hs_id`, `kp_hs_id`, `ks_hs_blind_id+<period>`, … (OpenSSH-Format). Read-only-Import aus C-Tor über `[storage.keystore.ctor.services."nick"] path = …`. **Kein Export-Kommando gefunden.** Bekannte Lücke: `ctor-migrate` migriert `authorized_clients` nicht (#2087). | https://tpo.pages.torproject.net/core/doc/rust/private/arti/subcommands/hss/index.html ; https://gitlab.torproject.org/tpo/core/arti/-/raw/main/crates/tor-hsservice/src/keys.rs ; https://gitlab.torproject.org/tpo/core/arti/-/issues/2087 |
| F2.11 | Offizieller Status: `doc/OnionService.md` nennt Onion-Service-Support „**suitable for testing and experimentation only**“ (Dokument selbst als veraltet markiert). | https://gitlab.torproject.org/tpo/core/arti/-/raw/main/doc/OnionService.md |
| F2.12 | Runtime: `tor-rtcompat` 0.47.0 bietet `tokio::TokioRustlsRuntime` (rustls `^0.23.21`); das `arti`-Binary wählt mit Feature `rustls` den Provider `rustls-aws-lc-rs` (alternativ `rustls-ring`). | https://docs.rs/tor-rtcompat/latest/tor_rtcompat/ ; https://docs.rs/crate/arti/latest/features |

**Folgerung für den Plan:** comnets Pin ist der aktuelle Stand; nichts zu
wechseln. Restricted Discovery ist produktionsreif genug für Profil
`onion-restricted`, **braucht aber eine Erweiterung von
`comnet-transport`** (dessen Builder setzt heute nur `nickname`,
`arti.rs:227-230`): Feature-Request mit `restricted_discovery`,
`rate_limit_at_intro`, `max_concurrent_streams_per_circuit` (letzteres
schließt die Rate-Limit-Lücke C6 teilweise). PoW bleibt **außen vor**.
HS-Key-Import geht programmatisch (`launch_onion_service_with_hsid`),
Export nur als Dateikopie des Keystores. Der „testing only“-Status gehört
in die Risikoliste.

## F3. systemd: programmatische Prüfung

| # | Befund | Quelle |
|---|---|---|
| F3.1 | `systemd-analyze verify FILE…` prüft unbekannte Sektionen/Direktiven, fehlende Abhängigkeiten, `Documentation=`-Manpages, fehlende/nicht ausführbare `ExecStart=`-Binaries; `--man=no` überspringt Manpage-Checks; `--generators` braucht meist root; `--recursive-errors=yes|one|no` steuert, welche Warnungen den Exit-Status setzen (ohne Option Exit 0); `$SYSTEMD_UNIT_PATH` überschreibt Suchpfade; `--instance=` für Templates. **Kein root nötig** für `verify`. | https://www.freedesktop.org/software/systemd/man/latest/systemd-analyze.html |
| F3.2 | `systemd-analyze security [UNIT]`: Exposure-Score 0.0–10.0 (hoch = wenig Sandboxing); `--offline=yes` analysiert Unit-Dateien **ohne PID 1**; `--threshold=`; `--security-policy=PATH` (JSON-Gewichte); `--json=pretty|short`. Erfasst nur systemd-eigene Mechanismen, nicht IPC-Zugriff. | ebd. |
| F3.3 | `systemd.exec(5)`: `DynamicUser=` impliziert `NoNewPrivileges`, `RestrictSUIDSGID`, `RemoveIPC`, `ProtectSystem=strict`, `ProtectHome=read-only` (UID 61184–65519, Verzeichnisse unter `/var/lib/private` per Symlink); `ProtectSystem=strict` = ganze Hierarchie read-only außer `/dev`, `/proc`, `/sys`; `StateDirectory`/`CacheDirectory`/`RuntimeDirectory` unter `/var/lib`, `/var/cache`, `/run`; `RestrictAddressFamilies=` Allowlist (Doku empfiehlt `AF_UNIX` einzuschließen; nur `socket()` wird beschränkt); `SystemCallFilter=@system-service` + `SystemCallErrorNumber=EPERM` als empfohlener Start; `PrivateNetwork=` trennt alle Socket-Familien vom Host (für harw-edge **nicht** nutzbar). | https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html |
| F3.4 | `systemd.syntax(7)`: `Key=Value` pro Sektion; Kommentare `#`/`;`; `\` am Zeilenende setzt fort (wird Leerzeichen); Quotes nur „where quoting is allowed“; C-Escapes; **leere Zuweisung setzt Listen zurück**; Booleans `1/yes/true/on` vs. `0/no/false/off`; Zeitspannen `2min 200ms`. | https://www.freedesktop.org/software/systemd/man/latest/systemd.syntax.html |
| F3.5 | `systemd.unit(5)`: Ladepfade `/etc/systemd/system` > `/run/systemd/system` > `/usr/lib/systemd/system`, Drop-ins `*.d/*.conf`; **unbekannte Optionen erzeugen nur eine Warnung**, die Unit lädt trotzdem (deshalb lohnt ein eigener strenger Prüfer). `systemd.service(5)`: `Type=simple|exec|forking|oneshot|dbus|notify|notify-reload|idle`, `ExecStart=`-Präfixe `@ - : + ! |`. | https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html ; https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html |

**Folgerung für den Plan:** `harw-unitfile` prüft **streng** (unbekannte
Direktive = Fehler, systemd selbst warnt nur); `doctor` ruft optional
`systemd-analyze verify --man=no --recursive-errors=yes` und
`systemd-analyze security --offline=yes --json=short` als externe
Zweitmeinung auf. `DynamicUser=` wird für harw-edge **nicht** verwendet
(arti prüft Verzeichnisrechte selbst; der HS-Key muss einen stabilen
Eigentümer haben), die implizierten Härtungen werden explizit gesetzt.

## F4. Crate-Stände (crates.io, 2026-11-01)

| Crate | Aktuell | Veröffentlicht | MSRV | Im Harwness-Lock |
|---|---|---|---|---|
| clap | 4.6.7 | 2026-09-14 | 1.85 | 4.6.1 / 4.6.7 |
| toml | 1.1.8+spec-1.1.0 | 2026-10-09 | 1.85 | 1.1.3 (workspace) |
| toml_edit | 0.25.17+spec-1.1.0 | 2026-10-09 | 1.85 | 0.25.15 |
| rustls | 0.23.46 | 2026-10-14 | 1.71 | 0.23.46 |
| tokio-rustls | 0.26.6 | 2026-09-28 | 1.71 | 0.26.x |
| hyper | 1.12.0 | 2026-10-06 | 1.63 | 1.11.1 |
| hyper-util | 0.1.21 | 2026-09-24 | 1.85 | 0.1.x |
| tower | 0.5.3 | 2026-01-12 | 1.64 | – |
| rcgen | 0.14.10 | 2026-08-28 | 1.88 | **neu** |
| x509-parser | 0.18.1 | 2026-02-05 | 1.67.1 | **neu** |

Quelle: `https://crates.io/api/v1/crates/<name>` (+ `/versions`,
`/<ver>/dependencies`).

- hyper 1.12: `server::conn::http1::Builder::serve_connection(io, service)` mit `I: Read + Write + Unpin`; `http2::Builder::new(exec)` (Features `http2` + `server`). — https://docs.rs/hyper/latest/hyper/server/conn/http1/struct.Builder.html
- hyper-util 0.1.21: `rt::TokioIo<T>` (Feature `tokio`) adaptiert tokio-I/O; **`server::conn::auto::Builder`** (`serve_connection`, `serve_connection_with_upgrades`, `http1_only`/`http2_only`) wählt HTTP/1.1 oder h2 pro Verbindung; **`service::TowerToHyperService`** (Feature `service`) adaptiert `tower_service::Service + Clone`. Beides deckt §7 des Plans ohne eigenen Adapter-Code. — https://docs.rs/hyper-util/latest/hyper_util/server/conn/auto/struct.Builder.html ; https://docs.rs/hyper-util/latest/hyper_util/service/struct.TowerToHyperService.html
- rcgen 0.14.10: `CertificateParams::new(san_list)`, `self_signed(&key)`, `signed_by(&pubkey, &Issuer)`; `Issuer::new`/`from_params`/`from_ca_cert_der` (Feature `x509-parser`)/`from_ca_cert_pem`; `is_ca`, `subject_alt_names: Vec<SanType>`, `key_usages`, `extended_key_usages`, `not_before/not_after`; Algorithmen `PKCS_ED25519`, `PKCS_ECDSA_P256_SHA256`, `PKCS_ECDSA_P384_SHA384` (P-521 und ML-DSA nur mit Backend `aws_lc_rs`); Backends `ring`/`aws_lc_rs`. Für Harwness: Backend **`aws_lc_rs`** wählen (kein zweiter Provider). — https://docs.rs/rcgen/latest/rcgen/

## F5. „Magic DNS“

| # | Befund | Quelle |
|---|---|---|
| F5.1 | **Tailscale MagicDNS** registriert Gerätenamen automatisch: FQDN `<machine>.<tailnet>.ts.net`, Kurznamen über Such-Domain; Default für Tailnets ab 2022-10-20. Auflösung lokal über den Stub-Resolver 100.100.100.100 / `fd7a:115c:a1e0::53` (Quad100), nicht über das Tailnet. | https://tailscale.com/kb/1081/magicdns ; https://tailscale.com/kb/1381/what-is-quad100 |
| F5.2 | **HTTPS im Tailnet:** `tailscale cert` holt ein **Let's-Encrypt**-Zertifikat für den `.ts.net`-Namen (DNS-01, Tailscale setzt den TXT); privater Schlüssel bleibt lokal; braucht MagicDNS + „HTTPS Certificates“; der Name landet in **öffentlichen CT-Logs** („Do not enable … if any of your machine names contain sensitive information“); 90 Tage, manuelle Erneuerung. | https://tailscale.com/kb/1153/enabling-https |
| F5.3 | **Headscale** unterstützt MagicDNS (`dns.magic_dns: true`, `dns.base_domain`, `extra_records` nur A/AAAA). `tailscale cert` gegen Headscale ist in **keiner** veröffentlichten Version (zuletzt v0.29.7, 2026-10-30), aber seit 2026-10-18 auf `main` (PR #3554: ACME DNS-01 über libdns-Provider inkl. Cloudflare, Konfiguration `dns.certificates`, `tls.acme.*`). | https://headscale.net/stable/ref/dns/ ; https://github.com/juanfont/headscale/pull/3554 ; https://github.com/juanfont/headscale/releases |

**Folgerung für den Plan:** Profil `tailnet` kann als Server-Zertifikat das
`tailscale cert`-Zertifikat nutzen (öffentlich vertrauenswürdig, Browser
glücklich), muss dann aber die CT-Sichtbarkeit des Maschinennamens in Kauf
nehmen; alternativ ein Leaf der eigenen Edge-CA (nur harw-Clients). harw-edge
löst MagicDNS-Namen **nicht** selbst auf und bindet an die Tailnet-IP.

## Nicht verifiziert

- Exakte Abstimmung/Datum von Ballot SC27v3 (cabforum.org war nicht erreichbar); belegt ist nur die BR-Historienzeile (wirksam 2020-03-27).
- Ob Tor Browser 15/16 Onion-`Alt-Svc` standardmäßig aktiviert hat (nur aus den aktiven Bug-Reports ableitbar).
- Ein arti-Kommando zum **Export** des HS-Identity-Keys (keines gefunden).
- Headscale-Release mit Node-Zertifikaten (bis 2026-11-01 nicht veröffentlicht).
