---
id: CONTRACT-KMS-USER-CRYPTO
title: KMS-Ausbau — Nutzer-Krypto, KEK-Provider und Identität auf Basis von crypt_guard_kms
status: draft
date: 2026-11-01
tags: [crypto, kms, kek, identity, auth-hub, secrets, contract-draft]
related:
  - Harwness_Crypto_Infrastructure_Masterplan_v2.md
  - ../../guides/kms-operations.md
  - ../../design/secrets-and-audit.md
  - ../../design/channel-ingress-comnet.md
  - ../67-containers/cell.md
---

# KMS-Ausbau: Nutzer-Krypto, KEK-Provider und Identität

> Status: **Contract-Draft** (Planung, nichts implementiert) · Stand: 2026-11-01

Dieser Vertrag überträgt den crypt_guard-Auftrag (neue Crates
`crypt_guard_kms` und `crypt_guard_pyo3`, OS-interne KEK-Speicher, eigene
Provider, mTLS/MCP/arti, selbstverschlüsselter Container) auf Harwness. Ziel:
Nutzer können **Dateien ver- und entschlüsseln, signieren und ihre Schlüssel
verwalten**, jeweils gebunden an ihre Identität.

Er setzt auf dem Masterplan v2 auf (§4 Key-Purpose, §5 kein Signing-Oracle,
§6.3 Secure-Socket, §16 Secrets-Migration) und ändert ihn nicht. Bei
Widersprüchen gilt der Masterplan, und der Widerspruch wird hier unter
„Offene Punkte" gemeldet.

---

## 1. Befundene Fakten (Ist-Zustand)

| # | Fakt | Beleg |
|---|---|---|
| F1 | `harw-auth-hub` ist ein lokaler KMS-Daemon über die CryptGuard-3.1-KMS-Routen, HTTP/1 auf `AF_UNIX`, Aufrufer per `SO_PEERCRED` → Principal, `NamespacePolicy`, keine Export-Route, Krypto auf eigenem Worker-Thread. | `harw-auth-hub/src/lib.rs:1-30`, `harw-auth-hub/src/auth.rs:3` |
| F2 | `SealedProvider` persistiert Seeds in einer XChaCha20-Poly1305-versiegelten Datei (`HAKS`) und musste **außerhalb von CryptGuard nachgebaut** werden, weil `crypt_guard_service::memory` privat ist. | `harw-auth-hub/src/sealed/mod.rs:3-20, 34-50` |
| F3 | Die Hub-KEK kommt aus genau einer Quelle: `kek_file` oder `kek_credential` (systemd-Credential, damit TPM2-fähig über `LoadCredentialEncrypted`). | `harw-auth-hub/src/config.rs:225, 555-577` |
| F4 | `harw-secrets` hat ein **zweites**, eigenes KEK-Konzept: `KekProvenance` = Key-Datei / OS-Keyring (`keyring` 3.6, Secret Service / Keychain / Credential Manager, optional) / Env-Seed. | `harw-secrets/src/kek.rs:42-46`, `harw-secrets/Cargo.toml:26, 36-39` |
| F5 | Der Store hat einen `generation`-Zähler; einen Anker außerhalb der Datei, der Rollbacks erkennt, habe ich nicht gefunden. | `harw-auth-hub/src/sealed/store.rs:515-620` |
| F6 | `docs/guides/kms-operations.md` §9 nennt noch „in memory only", obwohl `SealedProvider` und `key_store.kind = "sealed"` existieren (Doku-Drift). | `docs/guides/kms-operations.md:419-438` vs. F2/F3 |
| F7 | `harw-cli` hat keine Nutzerbefehle für Datei-Verschlüsselung, Signatur oder Schlüsselverwaltung; Krypto dient intern Provider-Secrets und DoD. | Suche in `harw-cli/src` (kein `encrypt`/`sign`/`keys`-Kommando) |
| F8 | `harw-security-hub` mappt die uid auf Principal, Tenant, Scope, Trust-Zone, `AuthStrength` und stellt `SecurityContext` aus. | `harw-security-hub/src/lib.rs:1-20` |
| F9 | Harwness bezieht crypt_guard von crates.io (3.1.0); `deny.toml` verbietet Git-Quellen. | `Cargo.toml:253-255`, `deny.toml:88-91` |
| F10 | `AuthStrength::HardwareBacked` ist an TPM / Secure Enclave gebunden und als „später" markiert. | `docs/planning/65-cloud-sessions/README.md:295` |

---

## 2. Vertragsspezifikation

### 2.1 Upstream zuerst: `crypt_guard_kms`

`crypt_guard_kms` (im crypt_guard-Repo) stellt **öffentlich** bereit, was
Harwness heute nachbaut:

- einen persistenten, versiegelten Provider (Ersatz für F2);
- `trait KekProvider` (laden, versiegeln, entsiegeln, Fähigkeiten melden);
- Plattform-Adapter für Peer-Identität (§2.3);
- optional: HTTPS+mTLS-, Streamable-HTTP-MCP- und arti-Frontends.

Harwness bindet `crypt_guard_kms` **als Bibliothek** ein, nicht als fertiges
Binary. `harw-auth-hub` behält nur das Harwness-Spezifische:
`harw.<purpose>/<owner>`-Namespaces, `SignTranscript`, SecurityHub-Contexts,
Audit über `harw-secrets`. Voraussetzung ist ein crates.io-Release (F9).

### 2.2 KEK: zwei Modi, die Wahl ist Pflicht

Ein einziges Konfigurationsschema ersetzt `KekSource` (F3) **und**
`KekProvenance` (F4). Es gibt **keinen Default**: fehlt `kek.mode`, lehnt der
strikte Parser die Config ab, und der Hub startet nicht.

```toml
[kek]
# Pflicht, ohne Default.
mode = "per-principal"   # "hub-derived" | "per-user" | "per-principal"

[kek.hub]                # Pflicht bei hub-derived und per-principal
provider   = "systemd-creds"   # systemd-creds | tpm2 | keychain | dpapi-machine | pkcs11 | file | command
credential = "harw-auth-hub-kek"

[kek.per_user]           # Pflicht bei per-user und per-principal
provider    = "os-keystore"    # secret-service | kernel-keyring | systemd-user-creds | keychain | dpapi-user
unlock_ttl  = "8h"             # Pflicht
when_locked = "deny"           # Pflicht: "deny" | "queue"

[[peers]]
uid       = 1000
principal = "alice"
kek_mode  = "per-user"   # nur bei mode = "per-principal", dort für jeden Peer Pflicht
```

Regeln:

| Modus | Store | Ableitung / Entsperren | Headless |
|---|---|---|---|
| `hub-derived` | eine Store-Datei | Unter-KEK je Principal: `HKDF(master_kek, info = "harw.kek.v1" ‖ principal)` | ja |
| `per-user` | eine Store-Datei **je Principal** | `harw kms unlock` läuft **als der Nutzer**, liest die KEK aus dessen OS-Keystore und übergibt sie über `secure.sock`; der Hub prüft die uid per `SO_PEERCRED` und hält die KEK nur im RAM (`Zeroizing`) bis `unlock_ttl` oder Logout | nein |
| `per-principal` | gemischt | jeder `[[peers]]`-Eintrag wählt `kek_mode` verpflichtend | gemischt |

- Fehlende Pflichtfelder, Felder des nicht gewählten Modus oder unbekannte
  Provider sind **Config-Fehler** (wie heute bei F3).
- `when_locked = "deny"` liefert für gesperrte Principals einen opaken Fehler;
  `"queue"` hält die Anfrage bis zum Entsperren oder bis zum Timeout.
- Ein Moduswechsel geschieht nur explizit:
  `harw keys migrate-kek --from <modus> --to <modus>`, mit Audit-Ereignis vor
  der Wirkung. Nie still.
- Eigene Provider (`pkcs11`, `command`) sind erlaubt und müssen ihre
  Fähigkeiten melden (z. B. `hardware_backed`). Nur dann darf der
  SecurityHub `AuthStrength::HardwareBacked` vergeben (F10).
- Rollback-Schutz (F5): Die `generation` wird zusätzlich außerhalb der Datei
  verankert (TPM-NV-Index, Keychain-Eintrag oder eine vom Provider versiegelte
  Hochwassermarke), nach dem Muster von `comnet-vault`s `GenerationGuard`.

### 2.3 Identität des Aufrufers

| Plattform | Anker | Zusätzliches, nur einschränkendes Merkmal |
|---|---|---|
| Linux | `SO_PEERCRED`-uid (heute, F1) | PID per `SO_PEERPIDFD` (Linux ≥ 6.5, statt PID-Lookup mit Wiederverwendungs-Race), daraus cgroup-v2-Pfad (`/proc/<pid>/cgroup` über den pidfd) → systemd-Unit / Agent-Slice |
| macOS | `getpeereid` / `LOCAL_PEERCRED` | Audit-Token → Code-Signing-Identität |
| Windows | Named Pipe → Client-Token-SID | `GetNamedPipeClientProcessId` → Image-Pfad / Signatur |

- cgroup-Merkmale werden nur ausgewertet, wenn der Pfad **nicht an den Nutzer
  delegiert** ist (Eigentümer root bzw. Hub-User). In einem delegierten
  Unterbaum kann der Nutzer Prozesse frei verschieben.
- Grants dürfen eine cgroup-Bedingung tragen
  (`cgroup = "/system.slice/harw-session@alice.service"`). Sie engt die
  uid-Freigabe nur ein und erweitert sie nie.

### 2.4 Nutzer-Operationen

Neue Operationen in `harw-operations` (`#[operation]`, Masterplan §12), damit
CLI, Web-UI und MCP sie ohne eigene Pfade bekommen:

| CLI | Operation | Hinweis |
|---|---|---|
| `harw crypt encrypt <in> -o <out> --key <ref>` | `crypt.encrypt_file` | streamend, crypt_guard-Dateiformat |
| `harw crypt decrypt <in> -o <out>` | `crypt.decrypt_file` | Key aus Header-Referenz |
| `harw crypt sign <in> [--detached]` | `crypt.sign_file` | nur über Purpose `harw.user-sign/<principal>` mit eigenem Transcript (§5: kein Signing-Oracle) |
| `harw crypt verify <in> --sig <sig>` | `crypt.verify_file` | öffentlich, ohne Freischaltung |
| `harw keys list/generate/rotate/disable/enable/destroy` | `keys.*` | bestehende Hub-Lifecycle-Routen |
| `harw keys export-public <ref>` | `keys.export_public` | nur öffentliche Schlüssel |
| `harw kms unlock / lock` | `kms.unlock` / `kms.lock` | nur `per-user` (§2.2) |

- Agents arbeiten nur mit `KeyRef`, sehen also nie Schlüsselbytes.
  Dateipfade laufen über `harw-authority::FileWriteScope`.
- Signieren oder Entschlüsseln, das ein Agent auslöst, braucht eine
  Freigabe des Nutzers (Approval über die Session bzw. den Channel).

### 2.5 Transport

- Primär bleibt `secure.sock` lokal (Masterplan §6.3).
- HTTPS mit mTLS, Streamable-HTTP-MCP und arti kommen aus `crypt_guard_kms`.
  Harwness gibt darüber **nur das Verteilen öffentlicher Schlüssel** frei
  (LAN bzw. Tor). Operationen mit privaten Keys bleiben lokal oder gehen über
  `harw-node-transport` mit mTLS.
- arti braucht einen eigenen DEC mit exaktem Pin (Vorbild: comnet DEC-004).

### 2.6 Hub als selbstverschlüsselter Container

Nach `docs/planning/67-containers`:

- Das Image ist per Digest gepinnt (`ImageDigest` akzeptiert nur `sha256`).
- Der versiegelte Store liegt im Volume.
- Die KEK ist **nie im Image**: Sie kommt vom Host-Provider (systemd-Credential,
  Podman-Secret, TPM).
- Re-Seal unter versionierten KEKs (`kek-<n>`, höchste = aktuell), wie
  `KekRing` in comnet.

### 2.7 Python

`crypt_guard_pyo3` gehört zu crypt_guard. Ein Python-Client für die Hub-API
ist für Harwness optional und nicht Teil der ersten Wellen.

---

## 3. Wellen

| Welle | Inhalt | Abhängigkeit |
|---|---|---|
| K0 | Doku-Drift F6 beheben, `KekSource` und `KekProvenance` gegenüberstellen | keine |
| K1 | gemeinsames KEK-Schema §2.2 mit Pflicht-`mode` | crypt_guard-Release mit `KekProvider` |
| K2 | `SealedProvider` gegen den Upstream-Provider tauschen; Kompatibilitätstests (HAKS lesbar) bleiben | K1 |
| K3 | Identität §2.3 (`SO_PEERPIDFD`, cgroup v2, macOS, Windows) | K1 |
| K4 | Nutzer-Operationen §2.4 (CLI, MCP, Web) | K2, K3 |
| K5 | Hub als Container §2.6, Rollback-Anker | K2 |
| K6 | öffentliche Schlüssel über LAN/Tor §2.5 | K4, arti-DEC |
| K7 | comnet-Channel (`../../design/channel-ingress-comnet.md`), unabhängig vom KMS-Umbau | eingefrorene agentd-API |

---

## 4. Offene Punkte

1. Welche Mindestversion von crypt_guard trägt `KekProvider` (3.2.0?). Abzustimmen mit dem crypt_guard-Chat.
2. Soll `harw-secrets` direkt `crypt_guard_kms::KekProvider` nutzen oder weiter über `DekWrapper` (V3) an den Hub delegieren?
3. `when_locked = "queue"`: maximale Wartezeit und Verhalten beim Neustart des Hubs.
4. Format des Rollback-Ankers je Provider (TPM-NV vs. Keychain vs. Datei).
5. Windows- und macOS-Hub: gleicher Daemon mit Named Pipe bzw. launchd-Socket, oder vorerst nur Client-Seite?

---

## 5. Tests

- Config: fehlendes `kek.mode` → Startfehler; Felder des nicht gewählten Modus → Fehler; `per-principal` ohne `kek_mode` an einem Peer → Fehler.
- `hub-derived`: verschiedene Principals bekommen verschiedene Unter-KEKs; Store von Alice ist mit Bobs Unter-KEK nicht zu öffnen.
- `per-user`: Operation vor `unlock` → opaker Fehler bzw. Warteschlange; nach `unlock_ttl` wieder gesperrt; `unlock` von fremder uid wird abgelehnt.
- `migrate-kek`: Audit-Ereignis vor der Wirkung; Abbruch in der Mitte lässt den alten Store lesbar.
- Rollback: Zurückspielen einer älteren Store-Datei wird erkannt.
- cgroup: Grant mit cgroup-Bedingung lehnt Aufrufe aus anderer Unit ab; delegierte cgroups werden ignoriert.
- Nutzer-Operationen: Roundtrip encrypt/decrypt für große Dateien (streamend); `sign` außerhalb von `harw.user-sign/*` wird abgelehnt.
- Kompatibilität: bestehende `HAKS`-Stores und V1/V2/V3-Secrets bleiben lesbar.
