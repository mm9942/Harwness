# `crypt_guard`-Anbindung in `harw-secrets`

Status: Ist (ab Welle W0b)

Dieses Dokument hält fest, warum `harw-secrets` `crypt_guard` als
crates.io-Version `3.0.1` bezieht, warum reines ML-KEM in dieser Version für
die deterministische Seed→KEK-Ableitung nicht nutzbar ist, und welche
Hybrid-KEM-Entscheidung Welle W0b daraus für `policy.rs`/`kek.rs`/
`envelope.rs` zieht. Es ersetzt keine Design-Entscheidung — es dokumentiert
den heutigen Stand, nachdem der parallele Umbau auf Hybrid-KEM
(`harw-secrets/src/{policy.rs,kek.rs,envelope.rs}`) abgeschlossen ist.

## 1. Warum crates.io `3.0.1` statt der früheren Pfad-Abhängigkeit

Vor W0b stand in `harw-secrets/Cargo.toml:10`:

```toml
crypt_guard = { version = "3.0.2", path = "../../crypt_guard" }
```

`/home/mia/projects/crypt_guard` existiert auf keinem bekannten Rechner
(per `find / -xdev` gegengeprüft, siehe
`harwness-analyse/reports/w4-facade-xtask-build-deploy.md` §5.1). Eine
Pfad-Abhängigkeit lässt Cargo das Zielmanifest schon beim
Workspace-Auflösen laden — das gilt auch für `cargo metadata --no-deps
--offline`, das an genau dieser fehlenden Datei abbricht. Damit waren
`make check/clippy/tests/gates/build`, `xtask` und jeder
`cargo`-basierte Workflow im gesamten Workspace blockiert, nicht nur die
Krypto-Pfade von `harw-secrets`.

`3.0.2` existiert außerdem nicht auf crates.io: Die Registry-API nennt
`3.0.1` (Stand 2026-09-13, veröffentlicht 18.07.2026) als neueste
Version; das GitHub-Repo `mm9942/crypt_guard` steht ebenfalls auf
`3.0.1`. Der `Cargo.lock`-Eintrag für `3.0.2` trug zudem **keine**
`source`/`checksum`-Zeile — ein sicheres Zeichen für ein reines
Pfad-Paket, nicht für eine veröffentlichte Version. Der bestehende
Kommentar im selben Manifest (`# crates.io dep, never a path dep`, siehe
`harw-secrets/Cargo.toml`) widersprach also bereits der tatsächlich
eingetragenen Pfad-Abhängigkeit — W0b löst den Widerspruch zugunsten der
crates.io-Version auf, nicht zugunsten des fehlenden Nachbar-Repos.

Damit ist `3.0.1` die einzige reproduzierbare, per `deny.toml`
prüfbare Quelle: eine Registry-Version mit `source`/`checksum`, ohne dass
`[sources] unknown-git = deny` gelockert werden müsste.

## 2. Was in `3.0.1` fehlt: kein Seed→Public-Key-Pfad für reines ML-KEM

Der vor W0b geschriebene Code in `kek.rs`/`envelope.rs` rief
`crypt_guard::pq_hpke::derive_recipient_key_pair(kem, &seed)` auf und
erwartete danach eine `.public_key()`-Methode, die direkt einen
öffentlichen Schlüssel liefert. Diese freie Funktion **existiert in
`3.0.1` nicht**:

```
$ grep -rn "derive_recipient_key_pair" crypt_guard-3.0.1/src/
(kein Treffer)
```

Tatsächlich vorhanden in
`crypt_guard-3.0.1/src/hpke_pq/mod.rs` (Modul
`draft_ietf_hpke_pq_05_full`, über `pq_hpke.rs:17-22` re-exportiert):

- `generate_recipient_key_pair(kem: Kem) -> Result<RecipientKeyPair, Error>`
  (Zeile 2594) — erzeugt das Schlüsselpaar **zufällig** über `OsRng`, nimmt
  keinen Seed entgegen. Für eine reproduzierbare KEK-Ableitung aus einem
  gespeicherten 32-Byte-Seed ungeeignet.
- `RecipientPrivateKey::from_seed_bytes(kem: Kem, seed: &[u8]) -> Result<Self, Error>`
  (Zeile 2525) — parst einen Seed deterministisch zu einem privaten
  Schlüssel, für **alle** `Kem`-Varianten (auch reines ML-KEM-512/768/1024).
- `RecipientPrivateKey::public_key(&self) -> Result<RecipientPublicKey, Error>`
  (Zeilen 2560–2576) — das **einzige** Mittel, aus einem seed-abgeleiteten
  privaten Schlüssel den öffentlichen zu gewinnen. Die Implementierung:

  ```rust
  pub fn public_key(&self) -> Result<RecipientPublicKey, Error> {
      match self.kem {
          Kem::MlKem768P256 | Kem::MlKem1024P384 | Kem::MlKem768X25519 => {
              Ok(RecipientPublicKey { kem: self.kem, inner: RecipientPublicKeyInner::Hybrid(
                  HybridPublicKey::derive(self.kem, self.as_seed_bytes())?) })
          }
          _ => Err(Error::InternalInvariant),
      }
  }
  ```

  Für die drei Hybrid-`Kem`-Varianten wird der öffentliche Schlüssel aus dem
  Seed abgeleitet (`HybridPublicKey::derive`). Für **jede andere** `Kem`-Variante
  — also genau `MlKem512`, `MlKem768`, `MlKem1024` (Zeilen 1724, 1726, 1728) —
  greift der `_`-Zweig und liefert `Err(Error::InternalInvariant)`.

Der Grund liegt in der zugrundeliegenden ML-KEM-Implementierung
(`libcrux_ml_kem`): Ihr FIPS-203-Seed-Format kodiert nur den privaten
Schlüssel; die zugehörige öffentliche Schlüsselableitung aus dem reinen
Seed ist dort nicht exponiert (`generate_key_pair`/`generate_key_pair_1024`
in `hpke_pq/mod.rs` erzeugen beide Hälften nur gemeinsam über `OsRng`,
kein Weg vom gespeicherten `MlKem*PrivateKey`-Seed zurück zum
öffentlichen Schlüssel). `crypt_guard` schließt diese Lücke nur für die
drei Hybrid-Suiten, deren klassischer Anteil (P-256/P-384/X25519) eine
öffentliche Skalarmultiplikation aus dem Seed erlaubt.

**Folge:** Mit `crypt_guard = "3.0.1"` lässt sich aus einem 32-Byte-KEK-Seed
für reines ML-KEM-512/768/1024 kein öffentlicher Schlüssel mehr ableiten —
weder deterministisch (keine solche Funktion existiert) noch über
`RecipientPrivateKey::public_key()` (schlägt für diese drei Varianten
immer mit `Error::InternalInvariant` fehl). Ein reiner ML-KEM-Pfad wäre nur
mit einer eigenen, ungetesteten Krypto-Ableitung außerhalb von
`crypt_guard` möglich gewesen — siehe Option D in
`harwness-analyse/reports/w4-facade-xtask-build-deploy.md` §5.2, dort
ausdrücklich als "nur mit Migrations-/Kompatibilitätsplan" bewertet, nicht
als W0b-Lösung übernommen.

## 3. Entscheidung: Hybrid-KEM als Default

W0b stellt `harw-secrets` auf die drei Hybrid-KEM-Suiten aus
`crypt_guard-3.0.1::hpke_pq::draft_ietf_hpke_pq_05_full::Kem` um, weil nur
für diese `RecipientPrivateKey::public_key()` einen öffentlichen Schlüssel
aus dem gespeicherten Seed liefert:

| `KemAlgo`-Variante | `crypt_guard::pq_hpke::Kem` | serde-Name | Draft-KEM-ID |
|---|---|---|---|
| Default | `MlKem1024P384` | `ml_kem_1024_p384` | `0x0051` |
| Alternative | `MlKem768P256` | `ml_kem_768_p256` | `0x0050` |
| Alternative | `MlKem768X25519` | `ml_kem_768_x25519` | `0x647a` |

`MlKem1024P384` bleibt der Default in `CryptoPolicy::strongest()` — höchste
Sicherheitsmarge, konsistent mit der bisherigen "ML-KEM-1024 ist Default"-
Linie, jetzt als Hybrid-Suite statt als reines ML-KEM.

Der bestehende 32-Byte-KEK-Seed (Key-File/Env-Seed/OS-Keyring liefern alle
exakt 32 Rohbytes, siehe `kek.rs`) passt unverändert zur Hybrid-Anforderung:
`hpke_pq/mod.rs:1698` definiert

```rust
const HYBRID_SEED_BYTES: usize = 32;
```

als die vom `HybridPrivateKey`/`HybridPublicKey`-Pfad erwartete Seedlänge.
Kein Formatwechsel bei der KEK-Provenienz nötig — nur der `Kem`-Wert, der
an `RecipientPrivateKey::from_seed_bytes` und danach an `.public_key()`
übergeben wird, ändert sich von einer reinen auf eine Hybrid-Variante.

## 4. Konsequenz: bestehende ML-KEM-512/768/1024-Envelopes werden unlesbar

Jeder Envelope, der vor W0b unter `KemAlgo::MlKem512`, `MlKem768` oder
`MlKem1024` versiegelt wurde, referenziert eine `Kem`-Variante, für die
`crypt_guard-3.0.1` keinen Seed→Public-Key-Pfad mehr hat (§2). Das
Öffnen eines solchen Legacy-Records mit dem heutigen Code ist also nicht
mehr möglich, unabhängig vom Envelope-Format (`dek_wrapped_v2` wie
`direct_hpke_v1`).

Das wird als eigener, typisierter Fehler geführt statt stillschweigend auf
`SecretsError::KekDerivation`/`Open` abzubilden, damit Aufrufer den Fall
"Envelope stammt aus der Zeit vor der Hybrid-Umstellung" von einer echten
Authentifizierungs- oder I/O-Störung unterscheiden können:

```rust
/// Ein Record referenziert eine reine ML-KEM-`KemAlgo`-Variante
/// (`ml_kem_512`/`ml_kem_768`/`ml_kem_1024`), für die crypt_guard 3.0.1
/// keine Seed→Public-Key-Ableitung mehr anbietet (nur die drei
/// Hybrid-Suiten). Legacy-Envelope, nicht mehr öffenbar.
#[msg("record '{id:?}' uses legacy KEM {kem:?}, unsupported since the crypt_guard 3.0.1 hybrid-KEM switch")]
UnsupportedLegacyKem { id: SecretId, kem: KemAlgo },
```

**Vorgehen, falls bereits alte Secrets existieren:** Es gibt keinen
automatischen Migrationspfad — ohne den alten, reinen ML-KEM-Seed lässt
sich der Klartext nicht mehr entschlüsseln, weil die Ableitung selbst
fehlt (nicht nur das Wire-Format). Betroffene Deployments müssen die
Secrets **neu anlegen**: alten Klartext aus der jeweiligen Quelle
(Provider-Dashboard, Passwort-Manager, Backup) erneut beziehen und mit der
laufenden Hybrid-`CryptoPolicy` neu versiegeln. Produktiv ist das nach
Stand der Analyse (`harwness-analyse/reports/w2-secrets-channels.md`
§1.5/§1.6) unkritisch: `harw-secrets` ist nur als Lesepfad für
Provider-Credentials verdrahtet, ein produktiver Bestand an
ML-KEM-512/768/1024-Envelopes vor W0b ist nicht dokumentiert.

## 5. Verweise

- `docs/setup/build-prerequisites.md` (wird parallel in Welle W0b
  angelegt) beschreibt die übrigen Voraussetzungen, um den Workspace
  wieder bauen zu können (crates.io-Auflösung, Lockfile-Bereinigung).
- `docs/design/crates-inventory.md` (nennt noch `crypt_guard | 2.0.3`) und
  `docs/design/secrets-and-audit.md` (nennt noch die crates.io-`2.0.3`-Linie
  und einen reinen-ML-KEM-`Encryptor`/`Decryptor`-Sketch) werden erst in
  Welle W16 auf den Hybrid-KEM-Stand nachgezogen. Bis dahin ist dieses
  Dokument die verbindliche Beschreibung der tatsächlichen
  `crypt_guard`-Anbindung.

## Nachtrag nach Review Z0 (Welle W0b)

- **Exakter Pin:** `harw-secrets/Cargo.toml` pinnt `crypt_guard = "=3.0.1"`. Ein stilles Update innerhalb von 3.x könnte die
  Schlüsselableitung ändern und alle versiegelten Secrets unlesbar machen; Updates nur bewusst mit neuem Known-Answer-Test.
- **Domain-Separation je KEM:** Aus dem 32-Byte-KEK-Seed wird pro Hybrid-KEM ein eigener 32-Byte-Seed abgeleitet
  (`derive_kem_seed`, SHA-256 mit KEM-spezifischem Kontext, `harw-secrets/src/kek.rs`). Dadurch teilen
  `ml_kem_768_p256` und `ml_kem_768_x25519` aus demselben Seed keinen ML-KEM-Schlüssel. `derive_secret_key` liefert den
  KEM-eigenen Seed, nicht den Wurzel-Seed.
- **Known-Answer-Test (Bless-Protokoll):** Der KAT in `kek.rs` vergleicht gegen `harw-secrets/tests/fixtures/kem_kat.txt`.
  Die Datei wird in der finalen cargo-Phase einmalig mit `HARW_BLESS=1 cargo test -p harw-secrets` erzeugt, geprüft und
  committet; fehlt sie ohne `HARW_BLESS`, schlägt der Test mit Anleitung fehl.
- **Legacy-Records:** Lesen/Rotieren liefert `SecretsError::UnsupportedLegacyKem`; die CLI (`harw-cli/src/secret_store.rs`)
  zeigt dazu den Hinweis, das Secret neu anzulegen. `rotate` verwendet pro Record dessen gespeicherten KEM.
