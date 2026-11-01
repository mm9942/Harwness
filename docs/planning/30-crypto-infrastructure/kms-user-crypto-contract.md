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

Abgestimmt mit der crypt_guard-Session (2026-11-01, Vertrag
`docs/plans/crypt-guard-kms-contract.md` in mm9942/crypt_guard#25):

- **`crypt_guard_keystore`** (eigene Crate): `KekProvider { backend(), open() -> Box<dyn Kek>, create(), destroy(), capabilities() }`
  und `Kek { id(), seal(&FileKey, aad), unseal(&WrappedFileKey, aad), derive(label) }`.
  Capabilities: `AtRestOnDisk`, `BoundToUser`, `BoundToMachine`, `HardwareBacked`,
  `RequiresPresence`, `SurvivesReboot`, `Exportable`. OS-Backends hinter
  Features (keyring 4.x, linux-keyutils, DPAPI, tss-esapi, Argon2-Passphrase).
  Die KEK muss nicht exportierbar sein; nur Software-KEKs bieten
  `ExportableKek::seed()` für Backups.
- **`crypt_guard_service`** bekommt additiv `SeedStore` und `SeedProvider<S>`
  (`InMemoryProvider` wird Alias); die Speicherlogik ist damit nicht mehr privat.
- **`crypt_guard_kms`**: `SealedProvider` über den Container `CGKS`
  (XChaCha20-Poly1305 oder AES-256-GCM-SIV, Rollback-Guard-Datei,
  atomares Schreiben, CAS-Rotation), Daemon, mTLS, MCP, arti.
- Mindestversion **3.2.0** auf crates.io; keine Git-Quelle nötig (F9).

Abbildung auf Harwness: `hub-derived` = `Kek::derive(principal)`;
`per-user` = `open()` unter der uid des Nutzers, `Kek` im RAM bis
`unlock_ttl`. Der Pflicht-`mode` bleibt eine Harwness-Config-Entscheidung.
`pkcs11` und `command` registriert Harwness selbst als eigene Provider
(`Arc<dyn KekProvider>`). Ein Leser für bestehende `HAKS`-Stores bleibt in
Harwness; neues Format ist `CGKS`.

Harwness bindet `crypt_guard_keystore` und `crypt_guard_kms` **als Bibliotheken** ein, nicht als fertiges
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
| Linux | `SO_PEERCRED`-uid (heute, F1) | cgroup-v2-Pfad → systemd-Unit / Agent-Slice, gebunden über die Sequenz unten (`SO_PEERPIDFD`, Linux ≥ 6.5) |
| macOS | `getpeereid` / `LOCAL_PEERCRED` | Audit-Token → Code-Signing-Identität |
| Windows | Named Pipe → Client-Token-SID | `GetNamedPipeClientProcessId` → Image-Pfad / Signatur |

**Bindung des cgroup-Merkmals an den Peer (Linux).** Die Prüfung muss unter
der ausgelieferten Unit laufen: `harw-auth-hub` läuft als `harw-auth` mit
`ProtectProc=invisible`, `ProcSubset=pid` und leeren Capability-Mengen
(`deploy/systemd/harw-auth-hub.service:27,49-50,61-62`). Fremde
`/proc/<pid>/`-Einträge sind damit unsichtbar, und `pidfd_send_signal`
liefert für fremde uids `EPERM`. Beides wird **nicht** aufgeweicht: kein
`CAP_KILL`, keine Proc-Sichtbarkeit nur für diese Prüfung.

Stattdessen prüft der Hub die **Bedingung des Grants** und liest nicht den
cgroup-Pfad des Peers. cgroupfs ist unter `ProtectControlGroups=yes`
nur schreibgeschützt, bleibt aber lesbar (`harw-auth-hub.service:46`). Die
Unit hat kein `PrivatePIDs`, PIDs sind also die des Host-Namespace. Genau
diese Sequenz, jeder Fehlschritt lehnt ab (fail closed):

1. `SO_PEERCRED` liefert uid und `pid`; `SO_PEERPIDFD` liefert `pidfd`.
   Fehlt `SO_PEERPIDFD` (Kernel < 6.5), werden Grants **mit**
   cgroup-Bedingung abgelehnt.
2. Nichtblockierendes `poll(pidfd, POLLIN, 0)`: lesbar heißt beendet,
   also ablehnen. Dafür braucht es keine Berechtigung (`pidfd_open(2)`).
3. Die Grant-Bedingung nennt eine **systemd-Unit**, keinen rohen Pfad
   (`unit = "harw-session@alice.service"`). Der Hub fragt den
   systemd-Manager über den System-Bus (D-Bus auf `AF_UNIX`, unter
   `RestrictAddressFamilies=AF_UNIX` erlaubt, `harw-auth-hub.service:52`)
   nach `ControlGroup` und `Delegate` der Unit. Akzeptiert wird nur eine
   geladene Unit mit `Delegate=no`. Ihr `ControlGroup` ist `C`.
4. **Nicht delegiert heißt: effektiv nicht beschreibbar**, nicht nur
   „Eigentümer root“ (cgroup-v2-Delegation ist über Schreibrechte auf
   Verzeichnis und `cgroup.procs` definiert, siehe Kernel-Doku
   „cgroup-v2 → Delegation“). Für `C` **und jeden Vorfahren bis zur Wurzel**
   gilt:
   - Verzeichnis, `cgroup.procs`, `cgroup.threads` und
     `cgroup.subtree_control` gehören root und haben keine Schreibbits für
     Gruppe oder andere;
   - es gibt keine POSIX-ACL (`system.posix_acl_access` bzw.
     `system.posix_acl_default` fehlen);
   - es gibt kein systemd-Delegationsmerkmal (`user.delegate` bzw.
     `trusted.delegate` fehlen).

   Weil der Kernel für eine Migration Schreibrecht auf `cgroup.procs` des
   Ziels **und** des gemeinsamen Vorfahren verlangt, kann der Peer dann
   weder hinein- noch hinauswechseln. Das gilt auch innerhalb eines
   cgroup-Namespace.
5. `/sys/fs/cgroup/C/cgroup.procs` mit Größenlimit lesen; `pid` muss darin
   stehen (exakte cgroup, keine Kinder).
6. Erneut `poll(pidfd, POLLIN, 0)`: lebt der Prozess noch, hält der pidfd
   die PID fest, und sie kann zwischen 1 und 5 nicht neu vergeben worden
   sein. Ist er beendet, wird abgelehnt.
7. Das Ergebnis gilt nur für diese Verbindung und wird nicht gecacht.

Restrisiko, bewusst akzeptiert: Nach Schritt 6 kann nur root bzw. systemd den
Prozess umhängen, weil Schritt 4 jedes Schreibrecht anderer ausschließt.
Ist der System-Bus nicht erreichbar, werden Grants mit Unit-Bedingung
abgelehnt.

Verworfen: `CAP_KILL` für `pidfd_send_signal`, `ProtectProc=default` für den
Hub, `/proc/<pid>/cgroup` über die numerische PID.

- Grants dürfen eine Unit-Bedingung tragen
  (`unit = "harw-session@alice.service"`). Sie engt die uid-Freigabe nur
  ein und erweitert sie nie. Eine Unit mit `Delegate=yes` oder ein
  beschreibbarer Pfad (Schritt 4) macht den Grant unbrauchbar und wird
  beim Laden der Config **und** bei jeder Prüfung abgelehnt.

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
- **Wo die Datei-I/O läuft:** im aufrufenden Harwness-Prozess unter der uid
  des Nutzers, nie im Hub. Der Hub sieht keine Nutzerdateien. Er wrappt
  nur den DEK (Verschlüsseln) bzw. signiert ein `SignTranscript` mit dem
  Digest der Datei (Signieren).
- **Eingabe-Zulassung** (alle Pfade, die gelesen werden: `<in>`, `--sig`):
  verlangt `Permission::ReadWorkspace`, Einschluss in die Workspace-Root und
  symlink-sichere Auflösung (kanonisieren relativ zu einem geöffneten
  Root-Verzeichnis per `openat2` mit `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS`;
  jeder Symlink im Pfad wird abgelehnt).
  `FileWriteScope` ist nur eine Schreibobergrenze und gilt hier nicht
  (`harw-authority/src/file_write_scope.rs:13-15`).
- **Ausgabe-Zulassung** (`-o <out>`, Detached-Signatur): verlangt
  `Permission::WriteWorkspace` **und** `FileWriteScope`
  (`harw-authority/src/lib.rs:34-43`). `FileWriteScope` ist rein lexikalisch;
  Aufrufer müssen zusätzlich unterhalb des Workspace auflösen und
  Symlink-Races verhindern (`harw-authority/src/file_write_scope.rs:1-3`).
  `FileWriteScope::exact_files` ist eine exakte Pfadobergrenze
  (`harw-authority/src/file_write_scope.rs:13-18,40-52`). Es darf also
  **kein zweiter Pfadname** entstehen, auch keine benannte Temporärdatei.
  Deshalb wird jede Ausgabe so geschrieben:
  1. Workspace-Root einmal als Verzeichnis-fd öffnen und festhalten.
  2. Elternverzeichnis relativ dazu per `openat2(RESOLVE_BENEATH |
     RESOLVE_NO_SYMLINKS)` öffnen.
  3. **Unbenannte** Staging-Inode im Eltern-fd:
     `openat(parent, ".", O_TMPFILE | O_WRONLY | O_CLOEXEC, 0600)`. Es gibt
     keinen Verzeichniseintrag. Weil die umask auch bei `O_TMPFILE` gilt
     (wirksam `0600 & ~umask`), folgt **vor dem ersten Schreiben**
     `fchmod(staging_fd, 0600)`. Schlägt das fehl, wird abgebrochen.
     Danach hineinschreiben und `fsync`.
  4. Commit **nur auf das zugelassene Ziel**:
     `linkat(AT_FDCWD, "/proc/self/fd/<n>", parent, "<name>", AT_SYMLINK_FOLLOW)`.
     Das läuft im Prozess des Nutzers, wo `/proc/self` sichtbar ist.
     `linkat` ersetzt nie: Existiert das Ziel, schlägt es mit `EEXIST` fehl,
     und die Operation lehnt ab. Danach `fsync` auf den Eltern-fd.
  5. Bei Fehler oder Absturz verschwindet die unbenannte Inode beim Schließen.
     Es bleibt kein Klartext-Eintrag zurück, und nichts muss aufgeräumt werden.
  - **Kein `--force`** in Welle K4: Ein bestehendes Ziel löscht der Nutzer
    vorher selbst. Ein atomares Ersetzen bräuchte einen zweiten Pfadnamen
    und damit eine eigene, eng abgeleitete Staging-Autorität. Das ist ein
    offener Punkt (§4).
  - Unterstützt das Dateisystem kein `O_TMPFILE` (`EOPNOTSUPP`/`EISDIR`),
    wird abgelehnt. Es gibt keinen Rückfall auf benannte Temporärdateien.
  - Die Zieldatei behält `0600`. Ein weiterer Modus nur, wenn die Config ihn
    ausdrücklich erlaubt, und nie für Entschlüsselungs-Ausgaben.
  - Restrisiko: Datenblöcke einer abgebrochenen Inode werden vom
    Dateisystem freigegeben, aber nicht überschrieben (kein Secure-Erase).
  - macOS und Windows haben kein `O_TMPFILE`. Die Ausgabe-Operationen sind
    dort bis zu einer eigenen Spezifikation nicht verfügbar (§4).
  Alle Schritte arbeiten auf festgehaltenen fds, nie auf Pfadstrings. Wird
  ein Pfadteil später durch einen Symlink ersetzt, ändert das das Ziel
  nicht.
- CLI, Web und MCP rufen dieselbe Operation mit derselben effektiven
  Autorität der Session auf; kein Einstiegspunkt hat eigene Pfadregeln.
- Signieren oder Entschlüsseln, das ein Agent auslöst, braucht zusätzlich
  eine Freigabe des Nutzers (Approval über die Session bzw. den Channel).
  Die Freigabe **erteilt keine Autorität**, die die Session nicht schon hat:
  Sie kann eine zugelassene Operation nur bestätigen, nie eine abgelehnte
  Ein- oder Ausgabe erlauben.

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
| K1 | gemeinsames KEK-Schema §2.2 mit Pflicht-`mode` | crypt_guard 3.2.0 (`crypt_guard_keystore`) |
| K2 | `SealedProvider` gegen `crypt_guard_kms::SealedProvider` (`CGKS`) tauschen; `HAKS`-Leser und Kompatibilitätstests bleiben | K1 |
| K3 | Identität §2.3 (`SO_PEERPIDFD`, cgroup v2, macOS, Windows) | K1 |
| K4 | Nutzer-Operationen §2.4 (CLI, MCP, Web) | K2, K3 |
| K5 | Hub als Container §2.6, Rollback-Anker | K2 |
| K6 | öffentliche Schlüssel über LAN/Tor §2.5 | K4, arti-DEC |
| K7 | comnet-Channel (`../../design/channel-ingress-comnet.md`), unabhängig vom KMS-Umbau | gemergter comnet-Commit/Tag mit agentd-v1-Vertrag (siehe dortiges §2.5) |

---

## 4. Offene Punkte

1. Migration `HAKS` → `CGKS`: einmalig beim Start oder per `harw keys migrate-store`; Rollback-Anker im TPM-NV ist upstream noch offen.
2. Soll `harw-secrets` direkt `crypt_guard_kms::KekProvider` nutzen oder weiter über `DekWrapper` (V3) an den Hub delegieren?
3. `when_locked = "queue"`: maximale Wartezeit und Verhalten beim Neustart des Hubs.
4. Format des Rollback-Ankers je Provider (TPM-NV vs. Keychain vs. Datei).
5. Windows- und macOS-Hub: gleicher Daemon mit Named Pipe bzw. launchd-Socket, oder vorerst nur Client-Seite?
6. Atomares Überschreiben (`--force`) bei exaktem Scope: eng abgeleitete Staging-Autorität mit Crash-Recovery, oder dauerhaft nicht anbieten?
7. Sichere Ausgabe auf macOS und Windows ohne `O_TMPFILE` (z. B. eigene Staging-Autorität mit Recovery).

---

## 5. Tests

- Config: fehlendes `kek.mode` → Startfehler; Felder des nicht gewählten Modus → Fehler; `per-principal` ohne `kek_mode` an einem Peer → Fehler.
- `hub-derived`: verschiedene Principals bekommen verschiedene Unter-KEKs; Store von Alice ist mit Bobs Unter-KEK nicht zu öffnen.
- `per-user`: Operation vor `unlock` → opaker Fehler bzw. Warteschlange; nach `unlock_ttl` wieder gesperrt; `unlock` von fremder uid wird abgelehnt.
- `migrate-kek`: Audit-Ereignis vor der Wirkung; Abbruch in der Mitte lässt den alten Store lesbar.
- Rollback: Zurückspielen einer älteren Store-Datei wird erkannt.
- cgroup: Grant mit cgroup-Bedingung lehnt Aufrufe aus anderer Unit ab; delegierte cgroups werden ignoriert.
- Delegation: root-eigene cgroup mit Gruppen-Schreibbit, mit ACL für den Peer, mit `user.delegate`-xattr oder Unit mit `Delegate=yes` → Grant abgelehnt; zur Gegenprobe ein Test, in dem der Peer in einer solchen cgroup sich nach dem zweiten `poll` selbst verschiebt, was die Ablehnung gerade verhindern soll; System-Bus nicht erreichbar → abgelehnt.
- cgroup-Bindung: Peer beendet sich zwischen Schritt 2 und 6 → abgelehnt; PID-Wiederverwendung (neuer Prozess unter derselben PID in `cgroup.procs`) → abgelehnt, weil der pidfd das Ende meldet; delegierte cgroup in einem Grant → abgelehnt; Kernel ohne `SO_PEERPIDFD` → Grants mit cgroup-Bedingung abgelehnt.
- Eingabe-Zulassung: Eingabe außerhalb der Workspace-Root → abgelehnt; Symlink im Workspace, der hinauszeigt → abgelehnt; Session ohne `ReadWorkspace` → abgelehnt, auch nach Nutzerfreigabe.
- Ausgabe-Zulassung: Ziel außerhalb von `FileWriteScope` oder ohne `WriteWorkspace` → abgelehnt, auch nach Nutzerfreigabe; gleiches Ergebnis über CLI, Web und MCP.
- Ausgabe-Symlinks: Elternverzeichnis ist ein Symlink nach außen → abgelehnt; Zieldatei ist ein Symlink → abgelehnt (`linkat` meldet `EEXIST`), nichts außerhalb wird überschrieben; Elternverzeichnis wird zwischen Öffnen und `linkat` gegen einen Symlink getauscht → Datei landet im festgehaltenen Verzeichnis oder der Vorgang schlägt fehl, nie außerhalb; vorhandenes Ziel → abgelehnt.
- Exakter Scope: Session mit `exact_files(["out/result"])`; während und nach der Operation zeigt das Verzeichnis-Listing keinen weiteren Namen als `out/result`.
- Absturz beim Entschlüsseln: Prozess wird mitten im Schreiben per `SIGKILL` beendet → kein neuer Verzeichniseintrag, `out/result` existiert nicht, kein Klartext-Rest im Workspace.
- Dateisystem ohne `O_TMPFILE` → Operation abgelehnt, keine benannte Temporärdatei angelegt.
- Modus: Ausgabe hat genau `0600`, sowohl bei umask `000` als auch bei umask `0777`; scheitert `fchmod`, wird abgebrochen, bevor geschrieben oder verlinkt wird.
- Unit-Integrationstest: Hub läuft mit genau den Sandbox-Einstellungen der ausgelieferten `harw-auth-hub.service`, Peer unter **anderer** uid. Grant mit passender cgroup-Bedingung → zugelassen; andere cgroup → abgelehnt; Peer beendet → abgelehnt. Kein `EPERM`-Pfad führt zur Zulassung.
- Nutzer-Operationen: Roundtrip encrypt/decrypt für große Dateien (streamend); `sign` außerhalb von `harw.user-sign/*` wird abgelehnt.
- Kompatibilität: bestehende `HAKS`-Stores und V1/V2/V3-Secrets bleiben lesbar.
