# C-FIXT — Capture-Manifest-Format + Materializer, echte Pi-Captures (`harw-dod-fixtures`)

Welle: W3 (Verträge). Rolle: focused-coding-task (sonnet, laut Plan `eventual-wandering-pebble.md` Teil B,
W3-Tabelle Zeile `C-FIXT | sonnet | harw-dod-fixtures/src/capture_manifest.rs(neu), captures/rpi5-6.18/*.json |
echte Pi-Captures (/sys, /proc lesend), Materializer mit echten Symlinks`; Querverweis W15-T1 verwendet einen
anderen Dateinamen/Pfad — s. Abschnitt „Abweichung von W15-T1" unten).

Build-Policy eingehalten: kein `cargo build/check/test/clippy/run/add`, kein `make`/`rustc`/`rust-analyzer`,
keine Git-Schreibbefehle, kein `sudo`. Erlaubt genutzt: lesende Shell-Befehle (`cat`, `readlink`, `ls -la`,
`date`) unter `/sys` und `/proc` dieses Raspberry Pi 5 zur Capture-Erhebung. Verifikation ausschließlich durch
Lesen (Signaturen, Imports, bestehende Fehlervarianten) — kein `cargo metadata` nötig, da keine neue
Abhängigkeit eingeführt wurde.

## Geänderte / neue Dateien

- **neu** `harw-dod-fixtures/src/capture_manifest.rs` — vollständiges Modul (Typen, Materializer, Loader,
  5 Tests), s. u.
- **neu** `harw-dod-fixtures/captures/rpi5-6.18/thermal.json`
- **neu** `harw-dod-fixtures/captures/rpi5-6.18/block.json`
- **neu** `harw-dod-fixtures/captures/rpi5-6.18/gpu.json`
- **neu** `harw-dod-fixtures/captures/rpi5-6.18/procnet.json`
- **neu** `harw-dod-fixtures/captures/rpi5-6.18/procmisc.json`
- **neu** `docs/remediation/ledger/W3/C-FIXT.md` (diese Datei)

Keine weiteren Dateien angefasst.

## Folgearbeit (außerhalb der eigenen Zuständigkeit)

`harw-dod-fixtures/src/lib.rs` ist **nicht** Eigentum dieses Agents. Ein Folge-Agent muss dort ergänzen:

```rust
pub mod capture_manifest;
```

(ein `pub use` der einzelnen Typen ist nicht nötig — `capture::CaptureReport` wird ebenfalls nur über den
Modulpfad re-exportiert, siehe `lib.rs` Kommentar über `pub use capture::CaptureReport;`). Erst nach dieser
Ergänzung lösen die `rust,no_run`-Doctests in `capture_manifest.rs` auf (sie referenzieren
`harw_dod_fixtures::capture_manifest::...` von außerhalb der Crate) und laufen die `#[cfg(test)]`-Tests der
Datei unter `cargo test -p harw-dod-fixtures`.

## Eingefrorene öffentliche Signaturen (exakt wie geschrieben)

```rust
// harw-dod-fixtures/src/capture_manifest.rs
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureManifest {
    pub kernel: String,
    pub captured_at: String,
    pub entries: Vec<CaptureEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureEntry {
    pub path: String,
    pub kind: CaptureEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureEntryKind {
    File { content: String },
    Symlink { target: String },
    Dir,
}

pub fn materialize(manifest: &CaptureManifest, root: &Path) -> std::io::Result<()>;
pub fn load(path: &Path) -> crate::error::FixturesResult<CaptureManifest>;
```

Deckt sich mit der Auftragsvorgabe `CaptureManifest { kernel, captured_at, entries }`,
`CaptureEntry { path, kind: File{content}|Symlink{target}|Dir }`,
`materialize(&manifest, &Path) -> io::Result<()>`, `load(path) -> Result<CaptureManifest, ..>`. `..` in der
Vorgabe wurde als der bereits vorhandene `FixturesResult`/`FixturesError` gelesen (s. Abschnitt „Fehlerbild"),
nicht als neuer dritter Fehlertyp — `error.rs` ist ohnehin nicht Eigentum dieses Agents.

## Fehlerbild — bewusst kein neuer Fehlertyp

`harw-dod-fixtures/src/error.rs` (nicht Eigentum dieses Agents) trägt bereits `FixturesError::Io(std::io::Error)`
und `FixturesError::Json(serde_json::Error)`, beide mit `#[from]` (API-Nachweis: `error.rs:76`, `error.rs:81`).
`load()` nutzt `fs::read_to_string(path)?` (→ `Io`) und `serde_json::from_str(&raw)?` (→ `Json`) — beide `?`
lösen ohne weitere Änderung an `error.rs` auf. `materialize()` gibt laut Auftragsvorgabe `io::Result<()>`
zurück (kein `FixturesResult`); ein abgelehnter `..`-Pfad wird als `io::Error` mit
`ErrorKind::InvalidInput` gemeldet (`capture_manifest.rs`, Funktion `resolve_under_root`).

## API-Nachweis (gelesen vor Verwendung)

- `#[derive(harw_macros::HarwError)]`, `#[from]`, `FixturesError::{Io,Json}`, `FixturesResult<T>`:
  `harw-dod-fixtures/src/error.rs:70-88` (bestehend, ungeändert).
- Bestehendes Modul-Muster für `use crate::error::FixturesResult;` und `?`-Propagation:
  `harw-dod-fixtures/src/fixture_io.rs:56,114-118,135-147` (bestehend, als Vorbild übernommen).
- `serde`/`serde_json` als reguläre (nicht Dev-)Abhängigkeiten bereits in
  `harw-dod-fixtures/Cargo.toml:16-17` — keine neue Abhängigkeit nötig, kein `dep-request`.
- `tempfile` als reguläre Abhängigkeit bereits in `harw-dod-fixtures/Cargo.toml:23-26` (Begründung dort: auch
  Bibliothekscode, nicht nur Tests) — für die Tests dieser Datei ausreichend, keine neue Abhängigkeit.
- `std::os::unix::fs::symlink`, `std::fs::{read_to_string, write, create_dir_all, remove_file,
  symlink_metadata, read_link}`, `std::path::{Path, PathBuf, Component}`, `std::io::{Error, ErrorKind, Result}`
  — Standardbibliothek, Linux-Target (Workspace baut nur für `x86_64`/`aarch64`-Linux, `unsafe_code = "forbid"`
  ist hier nicht betroffen: kein `unsafe`-Block verwendet).
- `env!("CARGO_MANIFEST_DIR")` in `test_load_reads_real_capture_file` zeigt auf
  `harw-dod-fixtures/` (Cargo-Standardverhalten je Crate) — `captures/rpi5-6.18/thermal.json` liegt direkt
  darunter.

## Abweichung von W15-T1 (nur benannt, nicht angefasst)

Der Plan nennt in der W15-Tabelle `harw-dod-fixtures/{captures/pi5-6.18/**,src/capture_tree.rs}` (anderer
Verzeichnisname `pi5-6.18` ohne „r", anderer Dateiname `capture_tree.rs`) für einen *anderen* Zweck
(sensorspezifische Fixture-Bäume für die `sensor_suite!`-Harness, Welle W15). Dieser Auftrag (C-FIXT, Welle
W3) hat ausdrücklich `harw-dod-fixtures/src/capture_manifest.rs` und `captures/rpi5-6.18/*.json` (mit „r") als
Owned Files vorgegeben — beide Namen bleiben nebeneinander bestehen, keine Datei dieses Auftrags wurde nach
`pi5-6.18/` oder `capture_tree.rs` verschoben. Ein W15-T1-Agent sollte beim Betreten dieses Verzeichnisses
diesen Ledger-Eintrag lesen, bevor er entscheidet, ob er das hier entstandene Manifestformat wiederverwendet
oder ein eigenes `capture_tree.rs`-Format daneben aufbaut.

## Erhobene Captures — Quelle und Zeitpunkt

Alle fünf `captures/rpi5-6.18/*.json`-Dateien stammen aus **einer** lesenden Sitzung auf diesem Raspberry Pi 5
(`kernel = "6.18.34+rpt-rpi-2712"`, via `uname -r`), `captured_at = "2026-09-13T18:33:35Z"` (via
`date -u +%Y-%m-%dT%H:%M:%SZ`). Erhoben ausschließlich mit `cat`/`readlink`/`ls -la` unter `/sys` und `/proc`
(siehe Build-Policy-Erlaubnis im Auftrag).

| Datei | Pfade | Inhalt |
|---|---|---|
| `thermal.json` | `/sys/class/thermal/thermal_zone0` (Symlink), `.../type`, `.../temp` | `readlink`-Ziel `../../devices/virtual/thermal/thermal_zone0`; `type=cpu-thermal`; `temp=69950` |
| `block.json` | `/sys/class/block/mmcblk0` (Symlink), `.../stat` | `readlink`-Ziel `../../devices/platform/soc@107c000000/1000fff000.mmc/mmc_host/mmc0/mmc0:aaaa/block/mmcblk0`; vollständige `stat`-Zeile |
| `gpu.json` | `/sys/class/drm/card0` (Symlink), `.../device/uevent` | V3D-GPU (`brcm,2712-v3d`); `readlink`-Ziel `../../devices/platform/axi/1002000000.v3d/drm/card0` |
| `procnet.json` | `/proc/net` (Dir), `/proc/net/{dev,tcp,udp,snmp}` | gekürzt auf repräsentative Zeilen (s. u. für Redaktion) |
| `procmisc.json` | `/proc/loadavg`, `/proc/meminfo` (erste 20 Zeilen) | unverändert, keine identifizierenden Inhalte |

`/sys/class/block/mmcblk0`s Symlink-Ziel enthält das Slot-Bezeichner-Segment `mmc0:aaaa` — das ist eine
generische SD/MMC-Bus-Adresse, kein Benutzer- oder Hostname, deshalb unverändert übernommen. Kein Pfad enthält
`/home` oder einen Benutzernamen.

## Redaktion in `procnet.json` (`/proc/net/{tcp,udp}`)

`/proc/net/tcp` und `/proc/net/udp` kodieren Adressen als little-endian Hex (`AABBCCDD` → IP `DD.CC.BB.AA`).
Jede Adresse, die diesen konkreten Host identifiziert (privates LAN, Tailscale-CGNAT, ein konkretes entferntes
HTTPS-Ziel), wurde durch eine Dokumentationsadresse aus `192.0.2.0/24` (RFC 5737, „TEST-NET-1") ersetzt.
`0.0.0.0`-Wildcards und `127.0.0.1` (Loopback) blieben unverändert, da sie nichts über den Host verraten.

| Original (dezimal) | Hex im Original | Rolle | Ersetzt durch | Hex ersetzt |
|---|---|---|---|---|
| `192.168.2.51` | `3302A8C0` | lokale LAN-Adresse (Pi selbst) | `192.0.2.10` | `0A0200C0` |
| `192.168.2.1` | `0102A8C0` | LAN-Gateway (DHCP-Server) | `192.0.2.1` | `010200C0` |
| `100.123.51.33` | `21337B64` | Tailscale-CGNAT-Adresse (Pi selbst) | `192.0.2.11` | `0B0200C0` |
| `160.79.104.10` | `0A684FA0` | entferntes HTTPS-Ziel (konkreter Server) | `192.0.2.2` | `020200C0` |
| `0.0.0.0` | `00000000` | Wildcard (keine Bindung) | unverändert | — |
| `127.0.0.1` | `0100007F` | Loopback | unverändert | — |

Alle übrigen Felder (Ports, UID, Inode-Nummern, Timer-/Queue-Zähler) sind unverändert übernommen — sie
identifizieren weder Host noch Nutzer und sind Teil des realistischen Wire-Formats, das die spätere
`D-SEC`/netcounters-Fixture (Welle W5) braucht.

## Tests im Modul (`harw-dod-fixtures/src/capture_manifest.rs`)

- `test_materialize_creates_real_symlink` — `materialize()` erzeugt einen echten Symlink
  (`fs::symlink_metadata(..).file_type().is_symlink()`) mit unverändertem Ziel sowie die begleitende Datei.
- `test_materialize_rejects_parent_dir_segment` — ein Pfad mit `..`-Segment liefert
  `io::ErrorKind::InvalidInput` und erzeugt nachweislich keine Datei außerhalb von `root`.
- `test_materialize_is_idempotent_for_repeated_symlink` — zweiter `materialize()`-Aufruf auf demselben `root`
  schlägt nicht fehl (Symlink wird vor dem Neuanlegen entfernt).
- `test_load_reads_real_capture_file` — lädt `captures/rpi5-6.18/thermal.json` über
  `env!("CARGO_MANIFEST_DIR")`, prüft `kernel` und das Vorhandensein des `temp`-Eintrags.
- `test_load_missing_file_is_io_error` — fehlende Datei liefert `FixturesError::Io`.

Erfüllt die im Auftrag geforderten drei Prüfungen (Symlink, `..` abgelehnt, echtes Capture-File laden) plus
zwei weitere für Idempotenz und den Fehlerpfad von `load()`.

## Offene Annahmen

- `CaptureEntryKind` ist intern über `type` getaggt (`#[serde(tag = "type", rename_all = "snake_case")]`) statt
  extern getaggt (`{"File": {...}}`), weil das im Auftrag genannte Kurzformat `File{content}|Symlink{target}|Dir`
  keine Tag-Konvention festlegt und die interne Form in den `captures/*.json`-Dateien besser lesbar ist
  (`{"type":"file","content":"..."}` statt `{"path":"...","kind":{"File":{"content":"..."}}}`). Ein
  Folge-Agent, der ein anderes Tag-Format erwartet, müsste sowohl `capture_manifest.rs` als auch alle fünf
  `captures/rpi5-6.18/*.json`-Dateien gemeinsam anpassen.
- `materialize()` überschreibt eine an der Zielstelle vorhandene reguläre Datei bzw. ersetzt einen vorhandenen
  Symlink, prüft aber nicht den Fall „an der Zielstelle liegt bereits ein nicht-leeres Verzeichnis" (dort würde
  `fs::remove_file`/`fs::write` mit einem regulären `io::Error` scheitern, nicht mit einer eigenen Meldung) —
  im heutigen Anwendungsfall (frisches `tempfile::tempdir()` als `root`) tritt das nicht auf.

## Geschlossene Register-IDs

F-202 (sysfs-Alias-Captures), F-005 (teilweise — Materializer-Grundlage für spätere Alias-Scope-Tests in
`harw-dod-cap`/`harw-dod-readfs`, Welle C-SCOPE).
