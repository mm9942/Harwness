---
id: CONTRACT-CHANNEL-COMNET
title: comnet als Channel — harw-channel-comnet über comnet-agentd
status: draft
date: 2026-11-01
tags: [channel, comnet, pqc, contract-draft]
related:
  - channel-ingress-telegram.md
  - ../planning/30-crypto-infrastructure/kms-user-crypto-contract.md
  - secrets-and-audit.md
---

# comnet als Channel in Harwness

> Status: **Contract-Draft** (Planung, nichts implementiert) · Stand: 2026-11-01

Scope: neue Crates `harw-channel-comnet` (Grenzlogik) und
`harw-channel-comnet-transport` (I/O), analog zu Telegram.
Depends on: `harw-channel`, `harw-authority`, `harw-session-store`,
`harw-secrets` (Audit-Mirror). **Keine** Abhängigkeit auf comnet-Crates.

---

## 1. Befundene Fakten

| # | Fakt | Beleg |
|---|---|---|
| F1 | `harw-channel` definiert den synchronen `ChannelAdapter` (`capabilities`, `derive_session_key`, `admit`, `render_outbound`, `run_ingress`). Ein Channel ist ein Perimeter und vergibt keine Rechte. | `harw-channel/src/adapter.rs:83-104`, `harw-channel/src/lib.rs:1-16` |
| F2 | Telegram trennt Grenzlogik (`harw-channel-telegram`) und I/O (`harw-channel-telegram-transport`); der Transport implementiert zusätzlich `MirrorTransport` für das Audit-Mirror. | `harw-channel-telegram/src/lib.rs`, `harw-channel-telegram-transport/src/lib.rs`, `harw-secrets/src/audit/mirror.rs:242` |
| F3 | `comnet-agentd` gibt AI-Agents Zugang zu comnet, ohne ihnen Keys zu geben: ein OS-User und ein Worker je Agent, eigene vaultd-Partition, eigene `agentd-<agent>.redb`; Zugang über UNIX-Socket (`SO_PEERCRED`) oder `127.0.0.1` mit audience-gebundenem Token; REST `/v1/*`, SSE `/v1/events`, WebSocket `comnet.agent.v1`, MCP `/mcp`. | comnet: `crates/comnet-agentd/src/main.rs:1-20` |
| F4 | Die REST-API kennt Kontakte, Konversationen, Nachrichten (list/send) und Dateien (upload/download, nur unter `file_root`). Jede Nachricht trägt `untrusted_content: true`, `sender_verified` und `sender_username`. IDs nie in Pfad oder Query. | comnet: `crates/comnet-agentd/src/rest.rs:1-30` |
| F5 | comnet bindet crypt_guard per Git-Rev ein (DEC-003); Harwness verbietet Git-Quellen und nutzt crypt_guard von crates.io. | comnet: `crates/comnet-vault/Cargo.toml`; Harwness: `deny.toml:88-91`, `Cargo.toml:253` |
| F6 | agentd setzt OS-User pro Agent und vaultd pro uid voraus, ist also Linux-first. | comnet: `crates/comnet-vault/src/lib.rs` („`comnet-vaultd` (Linux)") |

---

## 2. Vertragsspezifikation

### 2.1 Anbindung über die Prozessgrenze

```
harw-channel (Core)
 └─ harw-channel-comnet            Grenzlogik: Admission, Pairing, Approvals, Rate-Limits
     └─ harw-channel-comnet-transport
          │ hyper http1 über tokio::net::UnixStream (wie harw-infra-client)
          ▼
     comnet-agentd worker --agent harw-<name>   (eigener OS-User, eigene vault-Partition)
          ▼
     comnet ①②③ über apicon (PQ-AKE, E2E)
```

- Harwness hängt **nicht** an `comnet-client`. Begründung:
  1. Private comnet-Keys bleiben in `comnet-vault`.
  2. Keine Git-Quelle und kein zweites crypt_guard im Graph (F5).
  3. Die Prozessgrenze schützt beide Seiten.
- Der Transport spricht nur den UNIX-Socket des Agent-Workers. Der
  Loopback-Token-Weg bleibt ausgeschaltet, solange kein Bedarf besteht.
- Kein `reqwest`, kein `axum`: Der Transport nutzt `hyper` +
  `hyper-util`, die im Workspace schon vorhanden sind.

### 2.2 Abbildung auf `ChannelAdapter`

| harw-channel | comnet |
|---|---|
| `derive_session_key` | aus `conversation_id`; in Logs nur gehasht |
| `admit` | nur `sender_verified == true` **und** gepinnter Kontakt; sonst `Admission`-Ablehnung |
| Pairing-Code | Kontaktanfrage plus Code in der ersten Nachricht, geprüft über `PairingRegistry` |
| Approvals | comnet-Nachricht mit einmaligem Token und TTL (Muster `harw-channel-telegram/src/approval_tokens.rs`) |
| Anhänge | `/v1/files/*`; lokale Pfade über `FileWriteScope` **und** agentd-`file_root` |
| `untrusted_content` | wird in den Prompt-Injection-Schutz der Session übernommen |
| `run_ingress` | SSE `/v1/events` oder WebSocket `comnet.agent.v1`, mit Reconnect, Jitter und Nachholen (§2.2a) |
| `render_outbound` | Klartext; `ChannelCapabilities` ohne Markdown, bis comnet Formatierung festlegt |

### 2.2a Keine Ereignisse verlieren

agentd spielt Ereignisse **nicht** nach: Ein Reconnect mit `Last-Event-ID`
liefert `event: lagged` mit `missed: null`, ein Überlauf (Broadcast oder
Stream-Queue) ebenfalls `lagged` mit Anzahl; danach muss der Konsument den
Zustand über REST neu lesen (comnet: `crates/comnet-agentd/src/sse.rs:1-15`).
Daher:

- **Die heutige API reicht dafür nicht.** `ListMessagesArgs` kennt nur
  `conversation_id`, `before` (Sender-Zeitstempel in ms) und `limit`
  (höchstens 200). Ergebnisse kommen neueste zuerst, und weder `msg_id`
  noch der Event-Cursor werden akzeptiert (comnet:
  `crates/comnet-agentd/src/tools.rs`, `ListMessagesArgs`). Paging über
  Zeitstempel kann Nachrichten mit gleichem Zeitstempel überspringen.
  Deshalb ist ein lückenloser Cursor Teil des Upstream-Gates (§2.5, Punkt 5).
- Der Transport hält je Konversation einen dauerhaften Cursor (den
  stabilen Server-Cursor aus §2.5, nicht den Zeitstempel) im Session-Store.
- Nach **jedem** `lagged`-Ereignis und nach **jedem** Reconnect wird die
  Live-Zustellung angehalten. Für jede bekannte Konversation (und neu über
  `/v1/conversations`) holt `/v1/messages/list` mit `after = <cursor>`
  aufsteigend und seitenweise alles nach, bis `has_more = false`. Erst danach
  geht es live weiter.
- Unbekannter oder abgelaufener Cursor (Retention): Der Transport liest die
  Konversation vollständig neu ein. Danach meldet er der Session ein
  sichtbares Ereignis „mögliche Lücke“. Er macht nie still weiter.
- Live-Ereignisse, die während des Nachholens eintreffen, werden gepuffert
  und über `msg_id` dedupliziert.
- Ein unbekannter Ereignistyp oder ein `lagged`, das nicht verarbeitet werden
  kann, führt zum vollständigen Nachholen, nie zum Überspringen.

### 2.3 Konfiguration

```toml
[channels.comnet]
enabled = true
agent   = "harw-main"                               # Pflicht: agentd-Agentname
socket  = "/run/comnet/agentd/harw-main.sock"       # Pflicht, absolut
principal = "alice"                                 # Pflicht: verknüpfter Harwness-Principal
pinned_contacts = ["alice@example.org"]             # Pflicht, darf leer sein
allow_pairing = false                               # Pflicht
audit_mirror  = true                                # optional, Default false
```

Die Identitäten bleiben getrennt. Der Harwness-Principal wird nur über die
Config mit dem comnet-Agent-Account verknüpft; private Schlüssel werden nie
geteilt.

### 2.4 Audit-Mirror

`harw-channel-comnet-transport` implementiert `MirrorTransport` (F2). Damit
laufen die Checkpoints der Audit-Kette als E2E-PQ-verschlüsselte
comnet-Nachricht an einen festgelegten Kontakt. Das ist ein zweiter,
hostexterner Weg neben Telegram, mit derselben Fire-and-forget-Semantik.

### 2.5 Grenzpflicht auf comnet-Seite

Mit diesem Channel wird die agentd-REST/SSE/WS-API für comnet zu einer
**Grenze nach comnet-Regeln** (Impact-Liste vor Änderungen, Contract-Tests
auf beiden Seiten).

**Vorschlag, noch nicht verifiziert.** Die comnet-Orchestrator-Session hat
am 2026-11-01 Folgendes zugesagt. Im comnet-Repo existiert dazu noch kein
Vertrag: Es gibt keine `docs/plans/agentd-api-contract.md` und keinen
Versions-Header, und `/v1/healthz` liefert nur `{"ok":true}`
(comnet: `crates/comnet-agentd/src/rest.rs`). Eine Absprache zwischen
Sessions ist kein reproduzierbarer Beleg.

Vorgeschlagen:

- Die agentd-API wird als **v1** eingefroren: REST `/v1/*`, SSE `/v1/events`,
  WS-Subprotokoll `comnet.agent.v1` und die MCP-Tools.
- `GET /v1/healthz` meldet `api: 1` plus Minor. Jede Antwort trägt den
  Header `comnet-agent-api: 1.<minor>` (Name geplant).
- Innerhalb von v1 nur additive Änderungen. Brüche gehen nach `/v2`, mit
  vorheriger Impact-Liste (inklusive Harwness) und Parallelbetrieb.
- In v1 garantiert: `sender_verified`, `sender_username`,
  `untrusted_content`, `conversation_id`, `msg_id`; Dateizugriff bleibt auf
  `file_root` beschränkt.
- comnet liefert Golden-JSON-Fixtures für alle v1-Payloads. Harwness
  spiegelt dieselben Dateien für seine Contract-Tests.

Der Transport prüft beim Start `api == 1` und lehnt eine andere Major-Version
ab (fail closed).

**Gate für die Implementierung (K7).** Begonnen wird erst, wenn ein
**gemergter comnet-Commit oder Tag** feststeht, der Folgendes enthält:

1. `docs/plans/agentd-api-contract.md`;
2. die finale Versionsaushandlung (`/v1/healthz` mit `api`/Minor und den
   Header-Namen);
3. Golden-JSON-Fixtures für REST, SSE und WS;
4. grüne Producer-Contract-Tests in comnet;
5. ein lückenloser Nachrichten-Cursor in `/v1/messages/list`:
   - `after: Option<cursor>` mit einem stabilen, serverseitig monotonen
     Cursor pro Konversation (kein Zeitstempel);
   - aufsteigende Reihenfolge, `next_cursor` und `has_more`;
   - Live-Events tragen denselben Cursor;
   - unbekannter oder abgelaufener Cursor → expliziter Fehler (z. B.
     `cursor_unknown`) statt einer leeren Seite;
   - die Retention-Dauer ist dokumentiert.

Dieser Commit wird hier mit SHA eingetragen; Harwness spiegelt die Fixtures
von genau diesem Stand.

---

## 3. Offene Punkte

1. comnet-Commit/Tag für das Gate in §2.5 (noch offen), daraus Header-Name und Ablageort der gespiegelten Fixtures.
2. Gruppenkonversationen: Mention-Gate wie bei Telegram oder nur 1:1?
3. Ob SSE oder WebSocket der Standard-Ingress ist.
4. Formatierung (Markdown) in comnet-Nachrichten.
5. Später: öffentliche Harwness-Keys signiert über comnet an Kontakte verteilen (koppelt an K6 im KMS-Vertrag).

---

## 4. Tests

- Admission: unverifizierter Absender, nicht gepinnter Kontakt, abgelaufener Pairing-Code → abgelehnt.
- Session-Key: gleiche `conversation_id` → gleicher `SessionKey`; verschiedene → verschieden.
- Approval-Token: einmalig, TTL, falscher Absender → abgelehnt.
- Datei-Download außerhalb von `FileWriteScope` → abgelehnt, auch wenn agentd es erlauben würde.
- Contract-Test gegen aufgezeichnete agentd-Antworten (Golden Files), inklusive `deny_unknown_fields`-Verhalten.
- Reconnect: Abbruch des Event-Streams führt zu Wiederaufnahme ohne doppelte Zustellung (Dedup über Nachrichten-ID).
- Nachricht trifft **während** eines Verbindungsabbruchs ein → wird nach dem Reconnect über REST nachgeholt und genau einmal zugestellt.
- Queue-Überlauf (`EVENTS_QUEUE` voll, `lagged` mit Anzahl) → vollständiges Nachholen ab Cursor, keine Lücke.
- `lagged` mit `missed: null` nach `Last-Event-ID` → Nachholen, bevor die Live-Zustellung weiterläuft.
- Mehrere Nachrichten mit identischem Sender-Zeitstempel → alle genau einmal nachgeholt.
- Lücke größer als eine Seite (≥ 201 Nachrichten) → vollständig über mehrere Seiten nachgeholt, keine fehlt.
- Abgelaufener oder unbekannter Cursor → vollständiges Neueinlesen und sichtbares „mögliche Lücke“-Ereignis, kein stilles Weiterlaufen.
