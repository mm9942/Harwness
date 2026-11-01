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
| `run_ingress` | SSE `/v1/events` oder WebSocket `comnet.agent.v1`, mit Reconnect und Jitter |
| `render_outbound` | Klartext; `ChannelCapabilities` ohne Markdown, bis comnet Formatierung festlegt |

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
auf beiden Seiten). Das muss im comnet-Repo festgehalten werden, bevor die
Implementierung beginnt.

---

## 3. Offene Punkte

1. Einfrieren der agentd-API (Version, z. B. über `/v1/healthz` oder einen Header).
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
