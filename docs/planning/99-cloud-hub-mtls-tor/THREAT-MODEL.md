# PL-99 — Bedrohungsmodell: Cloud Hub, PQ-mTLS und Tor

> **Status:** Contract-Draft. Gehört zu [README.md](README.md).

## 1. Schutzgüter

| Gut | Wo | Schutzziel |
|---|---|---|
| ML-DSA-65-Knotenschlüssel | `harw-auth-hub` | Vertraulichkeit; der Prozess sieht ihn nie |
| Peer-Pins (`node-peers.conf`) | Profil | Integrität; nur der Operator ändert sie |
| Geräteregister (`node-devices.conf`) | Profil | Integrität; das Tier legt der Host fest |
| Pairing-Secret | Hub-State (nur Digest) | Vertraulichkeit, Einmaligkeit, kurze TTL |
| Onion-Identitätsschlüssel | `<profile>/tor/state` (0700) | Vertraulichkeit; daran hängt die stabile Adresse |
| Restricted-Discovery-Client-Keys | Gerät und Hub | Vertraulichkeit; nur gepairte Geräte finden den Dienst |
| Session-Frames (`harw.session.v1`) | innere PQ-TLS-Verbindung Gerät ↔ Zielknoten | Vertraulichkeit und Integrität **Ende-zu-Ende**, auch gegenüber dem Hub. Ohne die innere Verbindung (T5, Schritt 4) gäbe es diesen Schutz nicht: Die Frames sind heute JSON (`harw-session-ws/src/codec.rs:219-227`, `harw-protocol/src/session_wire.rs:494-501`) und nur durch den jeweiligen Kanal geschützt |
| Verzeichnis `SessionId → (node, generation)` | Hub | Integrität; Fencing gegen doppelte Owner |
| Metadaten (wer spricht mit wem, wann) | Netz | Vertraulichkeit, soweit Tor genutzt wird |

## 2. Angreifer

- **A1 – Netzbeobachter (passiv):** sieht TCP-Ströme, DNS und Zeiten.
- **A2 – aktiver Netzangreifer / MITM:** schiebt sich zwischen Gerät und Knoten.
- **A3 – fremder Internet-Client:** kennt eine Adresse (IP oder Onion), kennt aber keinen Schlüssel.
- **A4 – kompromittiertes Gerät:** besitzt einen gültigen, gepairten Schlüssel.
- **A5 – kompromittierter Hub:** Prozess oder Host des Cloud Hubs. Er kontrolliert die äußeren Verbindungen,
  das Relay, das Verzeichnis und die Routenvergabe, aber keine Geräte- oder Knotenschlüssel.
- **A6 – bösartiger Tor-Relay:** Guard, Middle, HSDir, Exit.
- **A7 – Agent/Prompt-Injection:** versucht über Werkzeuge Netz-Rechte auszuweiten.
- **A8 – Lieferkette:** kompromittierte Abhängigkeit (arti-Baum).

## 3. Bedrohungen und Gegenmaßnahmen

| # | Bedrohung | Angreifer | Heute | Maßnahme (Phase) |
|---|---|---|---|---|
| B1 | Signatur-Orakel: der Server signiert für jede bekannte Client-ID | A3 | **offen** (`handshake.rs:571`) | Handshake v2, Client zuerst (T0) |
| B2 | Slot-Erschöpfung, Slow-Loris | A3 | **offen** (nur 256 globale Slots) | Limits je Quelle, getrennte Pre-/Post-Auth-Slots, Lebensdauer (T0); bei Onion zusätzlich PoW (T2) |
| B3 | MITM liest IDs im ClientHello | A2 | teilweise (TLS-Kanal ohne Auth) | Bleibt bei v2 im verschlüsselten TLS; über Tor zusätzlich Ende-zu-Ende über Onion |
| B4 | Relay-MITM über zwei TLS-Sitzungen | A2 | **geschützt** (Exporter-Bindung im Transkript) | beibehalten |
| B5 | Replay eines Handshakes | A2 | **geschützt** (Replay-Cache, ±30 s) | beibehalten; Cache je Prozess reicht dank Exporter |
| B6 | Gestohlener Gerätschlüssel | A4 | Widerruf, aber Pins sind starr | Pins neu laden, Widerruf trennt live, Rotation (T0) |
| B7 | Pin- oder Register-Manipulation über Symlinks | lokal | teilweise (Server streng, Client locker) | überall `read_peer_pins`/`open_nofollow`, 0600 (T0) |
| B8 | Verlust des Knotenschlüssels nach Neustart | Betrieb | **offen** (AuthHub nur im Speicher) | versiegelter Keystore als Standard (T0) |
| B9 | Onion-Adresse wird bekannt → Probing | A3 | — | Restricted Discovery: Ohne Client-Key ist der Deskriptor nicht lesbar (T2) |
| B10 | Deanonymisierung des Onion-Dienstes | A6 | — | arti-Vanguards-Default, keine Exit-Abhängigkeit, ein Onion je Zweck; Restrisiko dokumentiert |
| B11 | Verknüpfung über Isolation hinweg | A1/A6 | — | Isolation je (Principal × Zweck), keine TLS-Resumption (T2) |
| B12 | DNS-Leak bei `.onion` | A1 | **offen** (`harw-egress` `lookup_host`) | `.onion` nie per DNS auflösen, nur über `ArtiDialer` (T6) |
| B13 | Stiller Fallback von Tor auf direkt | A1 | — | `PolicyDialer` fail-closed, kein Fallback (T2/T3) |
| B14 | Pairing-Secret erraten oder erneut abspielen | A3 | — | 128 Bit, nur Digest, TTL, Rate-Limit, Burn, Bindung an das Transkript, Freigabe durch den Operator (T4) |
| B15 | Gerät setzt sein Tier selbst | A4 | **geschützt** (Tier aus dem Register) | beibehalten; Pairing setzt das Tier hostseitig (T4) |
| B16 | Kompromittierter Hub erweitert Rechte | A5 | — | Der Knoten authentisiert das Gerät selbst (eigener Pin) und autorisiert selbst; die Hub-Prüfung ist nur ein Vorfilter (T5) |
| B22 | Kompromittierter Hub liest oder verändert Session-Frames | A5 | — (bei terminierendem Hub wäre das möglich) | Hub ist reines Relay; innere PQ-TLS + ML-DSA Ende-zu-Ende Gerät ↔ Knoten über die Route, AEAD-Integrität, Transkript gebunden an `(route_id, session_id, placement_generation)` (T5) |
| B23 | Hub vertauscht Routen, spielt alte Routen oder Generationen erneut ab, verwirft Frames | A5 | — | innerer Handshake bindet Route, Session und Generation; der Knoten prüft die Generation selbst; Replay-Cache und Zeitfenster innen. Verwerfen und Verzögern bleibt möglich (Verfügbarkeit, Restrisiko) |
| B24 | Hub schiebt eigene Schlüssel als Gerät oder Knoten unter | A5 | — | Pins nur aus Pairing mit Operator-Bestätigung bzw. Operator-Signatur (`PinEndorsement`); Hub darf nur vorschlagen (T4/T5) |
| B17 | Hub routet an den falschen Owner | A5/Bug | — | `placement_generation` als Fencing-Token, kein Failover in v1 (T5) |
| B18 | Agent erweitert Netz-Scope über Tor | A7 | — | `[network].tor` nur verengend, Child ⊆ Elternteil, `require_anonymity` je Rolle, Allowlist bleibt (T6) |
| B19 | Tailnet-Pfad umgeht die Knoten-Identität | A4 (Tailnet) | **offen** (`harw-tailscale`) | Tailnet nur als Underlay unter node-transport, oder Login → Principal mit eigenem Tier (T7) |
| B20 | Lieferkette arti | A8 | — | exakter Pin, `cargo deny`, Gate gegen `ring`, nur Ring A, monatlicher Review-Bump (T2) |
| B21 | Secrets in Logs | Betrieb | **geschützt** (redigierte Debug-Ausgaben) | `safelog`, Onion gekürzt, IDs gehasht (T7) |

## 4. Restrisiken

- Tor selbst ist nicht post-quantensicher (ntor-v3/hs-ntor, Ed25519-Onion-Schlüssel). Kompensiert wird das durch
  das innen liegende PQ-TLS + ML-DSA. Ein späterer Quantenangreifer kann zwar Tor-Metadaten aufdecken, aber keine
  Inhalte.
- arti hat kein Circuit-Padding, und hs-PoW ist experimentell.
- Ein globaler passiver Angreifer kann Zeitkorrelationen bilden.
- Die Hub-Verfügbarkeit ist ein Single Point of Failure, solange es in v1 kein Failover gibt.
- Ein kompromittierter Hub kann Routen verweigern, verzögern oder Verkehr verwerfen, und er sieht Metadaten:
  welches Gerät wann mit welcher Session spricht, Frame-Größen und Zeiten. Inhalte sieht er nicht und kann sie
  nicht verändern.
- Ein kompromittiertes, gepairtes Gerät bleibt bis zum Widerruf im Rahmen seines Tiers handlungsfähig.

## 5. Prüfbare Invarianten (Tests)

1. Ein unsigniertes oder falsch signiertes `ClientHello` (v2) löst keine AuthHub-Signatur aus.
2. Bei `require_anonymity` scheitert jeder Dial ohne Tor, bei der Konstruktion und bei jedem Aufruf.
3. `.onion` erreicht nie einen DNS-Resolver.
4. Ein entpinnter Peer verliert Live-Verbindungen innerhalb der Poll- bzw. Reload-Frist.
5. Ein bösartiges Test-Relay (mitlesen, Bytes kippen, Routen vertauschen, alte Generation wiederholen) sieht nie
   einen Session-Frame im Klartext, und jede Manipulation lässt die innere Verbindung scheitern oder führt zur
   Ablehnung durch den Knoten.
6. Ein vom Hub vorgeschlagener Pin wird ohne Operator-Bestätigung bzw. -Signatur nie übernommen.
7. Kein Netz-Scope eines Kindes ist breiter als der seines Elternteils, auch nicht mit `[network].tor`.
