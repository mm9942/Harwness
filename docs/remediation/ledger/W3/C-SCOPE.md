# W3 — C-SCOPE: sysfs-Alias-Wurzeln für den Sensor-Lesebereich (F-005, F-202)

Rolle focused-coding-task, Sicherheitsstufe hoch. BUILD-POLICY eingehalten: nichts gebaut/geprüft/getestet,
kein fmt-Schreiblauf (`cargo fmt` ist auf dem Pi nicht installiert, daher auch kein `--check` möglich), keine
git-Schreibbefehle. Einziger cargo-Aufruf: `cargo metadata --offline --no-deps --format-version 1` → Exit 0.
Verifikation durch Lesen.

Owned/geändert:
- `harw-dod-cap/src/scope.rs`
- `harw-dod-readfs/src/glob.rs`
- `harw-dod-readfs/src/read.rs` (nur Moduldoku + 3 Tests, keine Logikänderung)
- `harw-dod-readfs/src/error.rs`
- dieses Ledger (neu)

## 1. Befund und Ursache (F-005)

`ReadScope::open` und `glob::glob` kanonisierten den Kandidaten und prüften das kanonische Ziel gegen nicht
kanonisierte Klassenwurzeln. sysfs-Klasseneinträge sind Symlinks nach `/sys/devices/…` → jeder Treffer fiel aus dem
Scope → thermal/blockio/gpu in Produktion leer (`SourceUnavailable`). F-202: keine Symlink-Tests → Klasse
ungetestet.

## 2. Pi-Nachweis (RPi 5, Kernel `6.18.34+rpt-rpi-2712`, 2026-09-13, `readlink` / `readlink -f`)

| Klassenpfad | `readlink` (eine Ebene) | kanonisch |
|---|---|---|
| `/sys/class/thermal/thermal_zone0` | `../../devices/virtual/thermal/thermal_zone0` | `/sys/devices/virtual/thermal/thermal_zone0` |
| `/sys/class/thermal/cooling_device0` | `../../devices/virtual/thermal/cooling_device0` | `/sys/devices/virtual/thermal/cooling_device0` |
| `/sys/block/mmcblk0` | `../devices/platform/soc@107c000000/1000fff000.mmc/mmc_host/mmc0/mmc0:aaaa/block/mmcblk0` | identisch aufgelöst unter `/sys/devices/platform/…` |
| `/sys/block/loop0` | `../devices/virtual/block/loop0` | `/sys/devices/virtual/block/loop0` |
| `/sys/class/block/mmcblk0p1` | `../../devices/platform/soc@107c000000/…/block/mmcblk0/mmcblk0p1` | unter `/sys/devices/platform/…` |
| `/sys/class/drm/card0` | `../../devices/platform/axi/1002000000.v3d/drm/card0` | `/sys/devices/platform/axi/1002000000.v3d/drm/card0` |
| `/sys/class/drm/card1` | `../../devices/platform/axi/axi:gpu/drm/card1` | `/sys/devices/platform/axi/axi:gpu/drm/card1` |
| `/sys/class/drm/card1-HDMI-A-1` | `../../devices/platform/axi/axi:gpu/drm/card1/card1-HDMI-A-1` | unter `/sys/devices/…` |
| `/sys/class/drm/card1/device` (**zweite, tiefere** Ebene) | `../../../axi:gpu` | `/sys/devices/platform/axi/axi:gpu` |
| `/sys/class/drm/card1/subsystem` | `../../../../../../class/drm` | `/sys/class/drm` |
| `/sys/class/thermal/thermal_zone0/subsystem` | `../../../../class/thermal` | `/sys/class/thermal` |
| `/sys/class/net/eth0` | `../../devices/platform/axi/1000120000.pcie/1f00100000.ethernet/net/eth0` | unter `/sys/devices/…` |

`/sys`, `/sys/class`, `/sys/class/thermal`, `/sys/class/drm`, `/sys/block`, `/sys/devices` sind echte Verzeichnisse
(`ls -ld`). `/sys/class/drm/version` ist eine reguläre Datei (kein Symlink) im Klassenverzeichnis.
Folgerung: Klassen-Einträge sind **genau eine** Symlink-Ebene, Ziel unter `/sys/devices`; tiefere Symlinks
(`device`, `subsystem`) bleiben unter `/sys/devices` bzw. zeigen zurück ins Klassenverzeichnis.

## 3. Neue/geänderte öffentliche Signaturen (exakt)

`harw-dod-cap/src/scope.rs` (Modul `pub mod scope`; `lib.rs` re-exportiert weiterhin nur `ReadScope` →
neue Typen über `harw_dod_cap::scope::{AliasRoot, AliasRootError, SYSFS_DEVICES}`):

```rust
pub const SYSFS_DEVICES: &str = "/sys/devices";

#[derive(Debug, HarwError)]
pub enum AliasRootError {
    DeclaredNotNormalized { path: String },
    ResolvedPrefixNotNormalized { path: String },
    FilesystemRoot { which: &'static str },   // "declared path" | "resolved prefix"
}
// Derive erzeugt zusätzlich: pub type AliasRootResult<T> = Result<T, AliasRootError>;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AliasRoot { declared: PathBuf, resolved_prefix: PathBuf }   // Felder privat
impl AliasRoot {
    pub fn new(declared: PathBuf, resolved_prefix: PathBuf) -> Result<Self, AliasRootError>;
    pub fn sysfs_class(declared: PathBuf) -> Result<Self, AliasRootError>; // resolved_prefix = SYSFS_DEVICES
    #[must_use] pub fn declared(&self) -> &Path;
    #[must_use] pub fn resolved_prefix(&self) -> &Path;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadScope { roots: BTreeSet<PathBuf>, aliases: BTreeSet<AliasRoot> }
impl ReadScope {
    #[must_use] pub fn from_roots(roots: impl IntoIterator<Item = PathBuf>) -> Self;              // unverändert
    #[must_use] pub fn from_roots_and_aliases(roots: impl IntoIterator<Item = PathBuf>,
                                              aliases: impl IntoIterator<Item = AliasRoot>) -> Self; // NEU
    pub fn roots(&self) -> impl Iterator<Item = &Path>;          // GEÄNDERT: einfache Wurzeln, dann Alias-declared (ohne Duplikate)
    pub fn alias_roots(&self) -> impl Iterator<Item = &AliasRoot>; // NEU
    #[must_use] pub fn intersection(&self, other: &Self) -> Self;  // + exakter Schnitt der Aliases; #[must_use] neu
    #[must_use] pub fn allows(&self, path: &Path) -> bool;         // GEÄNDERT: `..` → false; Alias-declared zählt
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, SensorError>; // NEU (kanonischer, geprüfter Pfad)
    pub fn open(&self, path: &Path) -> Result<File, SensorError>;  // Semantik: resolve + File::open(canonical)
}
```

`harw-dod-readfs/src/glob.rs`:

```rust
pub const MAX_GLOB_COMPONENTS: usize = 32;
pub const MAX_GLOB_CANDIDATES: usize = 4096;
pub fn glob(scope: &ReadScope, pattern: &str) -> ReadFsResult<Vec<PathBuf>>; // Signatur unverändert, Semantik s. §4
```

`harw-dod-readfs/src/error.rs` — neue Variante:

```rust
#[msg("Glob-Muster '{pattern}' überschreitet die Grenze für {limit_name} ({limit})")]
GlobLimitExceeded { pattern: String, limit_name: &'static str /* "components" | "candidates" */, limit: usize },
```

## 4. Semantik (Sicherheitsregeln)

`ReadScope::resolve(path)`:
1. `path` enthält `Component::ParentDir` → `OutsideScope` (vor jeder I/O).
2. `canonical = path.canonicalize()` → sonst `SensorError::Io`.
3. `canonical` unter einer **einfachen** Wurzel (`Path::starts_with`, komponentenweise) → `Ok(canonical)`
   (Verhalten für einfache Wurzeln unverändert).
4. Für jede Alias-Wurzel mit `path.starts_with(declared)` (lexikalisch):
   - `rest` leer → nur ok, wenn `canonical == declared`.
   - erster Rest-Eintrag `e`, `entry = declared/e`: `symlink_metadata(entry)`, `entry_canonical = canonicalize(entry)`.
     - Symlink: `read_link` → **eine** Ebene lexikalisch relativ zu `declared` auflösen (`..` poppt; über `/` hinaus →
       abgelehnt). Ergebnis muss echt unter `resolved_prefix` liegen (≠ Präfix selbst) **und** `== entry_canonical`
       sein. Abweichung = doppelter Symlink oder symlinktes Zwischenverzeichnis → abgelehnt.
     - kein Symlink: `entry_canonical == entry` (sonst ist `declared` selbst symlinkt) → sonst abgelehnt.
   - `canonical` muss unter `resolved_prefix` **oder** `declared` liegen (tiefere Symlinks wie `card1/device` ok,
     nach außen nie).
5. sonst `OutsideScope`. `resolved_prefix` ist **keine** Anfragewurzel: `/sys/devices/…` direkt → `OutsideScope`.

`AliasRoot::new`: beide Pfade absolut, nur `RootDir` + `Normal`-Komponenten, nicht `/`; wird komponentenweise
normalisiert (`/sys//block/` → `/sys/block`).

`glob`:
- Musterhygiene wie bisher + `components.len() > 32` → `GlobLimitExceeded{"components"}`.
- Kandidaten werden nach jedem Schritt beschnitten: nur Namen, die lexikalisch Vorfahr einer `roots()`-Wurzel sind
  oder darunter liegen (schließt den W1-R11-Existenz-Seitenkanal „Suche ab `/`“ weitgehend).
- `read_dir` nur auf echten lexikalischen Vorfahren einer Wurzel oder auf Verzeichnissen, die `resolve` bestehen →
  kein Listen hinter einem Symlink nach außen.
- mehr als 4096 Kandidaten in einem Schritt → `GlobLimitExceeded{"candidates"}`.
- Endfilter `scope.resolve(candidate).is_ok()` (statt `canonicalize` + `allows`); Rückgabe weiterhin die
  **angefragten** (nicht kanonischen) Pfade, sortiert — Sensoren leiten Gerätenamen aus `parent().file_name()` ab.

Verhaltensänderungen für bestehende Aufrufer (bewusst):
- `allows` lehnt `..` ab (vorher lexikalisch `true` möglich).
- `open`/`resolve` lehnen `..` vor I/O ab (vorher kanonisiert und ggf. zugelassen).
- `glob` liefert keine Treffer mehr, deren **Name** neben der Wurzel liegt, auch wenn das Ziel kanonisch in der
  Wurzel läge (Test `test_glob_prunes_sibling_names_outside_root`). Produktionsmuster sind immer
  `roots().next()` + Suffix und damit nicht betroffen.

## 5. Tests (neu; alle mit echten Symlinks im Tempdir, Tempdir vorher kanonisiert)

`harw-dod-cap/src/scope.rs`:
`test_intersection_keeps_only_identical_alias_roots`, `test_allows_rejects_parent_dir_component`,
`test_allows_alias_declared_but_not_resolved_prefix`, `test_roots_lists_plain_roots_then_alias_declared_without_duplicates`,
`test_alias_root_new_normalizes_trailing_and_double_slashes`, `test_alias_root_new_rejects_relative_declared`,
`test_alias_root_new_rejects_parent_dir_in_prefix`, `test_alias_root_new_rejects_filesystem_root_prefix`,
`test_alias_root_sysfs_class_uses_sys_devices`, `test_lexical_resolve_relative_sysfs_target`,
`test_lexical_resolve_rejects_escape_above_root`,
`test_resolve_plain_class_root_rejects_sysfs_symlink_regression_f005` (reproduziert F-005),
`test_open_alias_class_entry_into_resolved_prefix_reads_value` (erlaubter Alias ok),
`test_resolve_alias_deeper_symlink_inside_prefix_is_admitted` (drm `card1/device`, Pi-Struktur),
`test_resolve_alias_regular_class_entry_is_admitted` (drm `version`),
`test_resolve_alias_pointing_outside_prefix_is_rejected` (Alias nach außerhalb),
`test_resolve_alias_double_symlink_is_rejected` (doppelter Symlink),
`test_resolve_alias_target_through_symlinked_directory_outside_is_rejected`,
`test_resolve_alias_deep_symlink_outside_is_rejected`,
`test_resolve_alias_parent_dir_component_is_rejected` (`..`),
`test_resolve_direct_resolved_prefix_path_is_rejected`, `test_resolve_alias_missing_entry_returns_io`.
Bestehende Tests unverändert erhalten.

`harw-dod-readfs/src/glob.rs`:
`test_glob_rejects_pattern_over_component_limit`, `test_glob_rejects_more_candidates_than_limit` (legt 4097 leere
Dateien an), `test_glob_prunes_sibling_names_outside_root`, `test_glob_alias_root_finds_class_symlinks_regression_f005`
(plain leer, Alias findet beide Zonen), `test_glob_alias_root_excludes_entry_pointing_outside`,
`test_glob_alias_root_excludes_double_symlink`.

`harw-dod-readfs/src/read.rs`:
`test_parse_i64_through_alias_root_reads_sysfs_class_symlink`, `test_parse_i64_plain_class_root_rejects_sysfs_symlink`,
`test_read_to_string_alias_rejects_parent_dir_escape`.

`harw-dod-readfs/src/error.rs`: `test_glob_limit_exceeded_message_contains_pattern_and_limit`.

## 6. API-Nachweise (gelesen)

- `#[derive(HarwError)]`, Attribute `msg`/`from`: `harw-macros/src/lib.rs:114`; Named-Field-Display
  `harw-macros/src/error.rs:147-167`; `<Prefix>Result`-Alias bei Suffix `Error`: `harw-macros/src/error.rs:211-224`.
- `SensorError::{OutsideScope, Io(#[from] std::io::Error), ToolFault}`: `harw-dod-cap/src/error.rs:65-125`.
- `harw-dod-cap` hängt an `harw-macros` (`harw-dod-cap/Cargo.toml`), `tempfile` dev-dep vorhanden;
  `harw-dod-readfs` hat `tempfile` dev-dep und `harw-dod-cap`.
- std: `Path::{canonicalize, starts_with, strip_prefix, components, is_absolute, parent}`, `PathBuf::{pop, push}`,
  `std::fs::{symlink_metadata, read_link, read_dir}`, `std::os::unix::fs::symlink` — alle ≤ MSRV 1.85; keine
  let-chains verwendet.
- Workspace-Lints: nur `unsafe_code = "forbid"` (`Cargo.toml [workspace.lints.rust]`).

## 7. Aufrufer der geänderten API (grep, außerhalb der Zuständigkeit)

### 7.1 Kompilierbruch durch neue Variante `ReadFsError::GlobLimitExceeded` (Regel 7, erschöpfende `match`)
Alle folgenden `match` sind ohne Wildcard und brauchen einen Arm (Empfehlung: → `SensorError::MalformedSource`
bzw. `ToolFault`; Muster nie übernehmen):
- `harw-dod-thermal/src/sensor.rs:347-354` (`map_readfs_err`)
- `harw-dod-blockio/src/sensor.rs:542-549`
- `harw-dod-gpu/src/sensor.rs:456-463`
- `harw-dod-cgroup/src/sensor.rs:532-539`
- `harw-dod-listener/src/procnet.rs:170-179`
- `harw-dod-scanreport/src/sensor.rs:234-243` (`map_read_fs_error`)
- `harw-dod-netcounters/src/sensor.rs:297-311` (inline-`match` in `poll`)
- `harw-macros/src/sensor_source.rs:688-700` (generiertes `map_readfs_err` für alle `#[derive(SensorSource)]`-Sensoren)

**Bis diese Arme ergänzt sind, kompilieren die genannten Crates nicht.**

### 7.2 Fachliche Folgearbeit W5 D-HW (Scope-/glob-Nutzer)
- `harw-sentinel/src/sensors.rs:252,277,292`: `thermal_scope`/`blockio_scope`/`gpu_scope` per
  `ReadScope::from_roots_and_aliases(Vec::new(), [AliasRoot::sysfs_class(root)?])` bauen (heute `from_roots`
  → F-005 bleibt bis dahin in Produktion offen). `AliasRootError` beim Start behandeln.
- `harw-sentinel/src/sandbox.rs:387-390`: Landlock-Lesewurzeln um `/sys/devices` (read-only) ergänzen, sonst
  blockiert Landlock das aufgelöste Ziel zusätzlich.
- `harw-dod-thermal/src/sensor.rs:267-273`, `harw-dod-blockio/src/sensor.rs:270-277`,
  `harw-dod-gpu/src/sensor.rs:352-359`, `harw-macros/src/sensor_source.rs:584-597`,
  `harw-dod-scanreport/src/sensor.rs:175-178`, `harw-dod-netcounters/src/sensor.rs:292`,
  `harw-dod-cpu/src/sensor.rs:219`, `harw-dod-memory/src/sensor.rs:253`,
  `harw-dod-fixtures/{src/capture.rs:372, tests/*.rs}`: nutzen `roots().next()` als Glob-Basis — mit Alias-Bereich
  liefert das `declared`, also kompatibel; keine Änderung nötig, aber Fixture-Tests mit Alias-Bäumen ergänzen.
- `harw-dod-gpu`: `card*/device` matcht auch `card1-HDMI-A-1/device -> ../../card1` (auf dem Pi belegt, kanonisch
  `/sys/devices/platform/axi/axi:gpu/drm/card1`, liegt unter `/sys/devices` → wird zugelassen). Doppelzählung
  bleibt fachlicher Befund (w2-dod-hw-collectors), jetzt nicht mehr vom Scope-Bug überdeckt.
- `harw-dod-fixtures` (C-FIXT, parallel): Materializer mit echten Symlinks muss `sys/devices` mit anlegen und die
  Harness-Scopes für thermal/blockio/gpu als Alias-Scopes bauen (F-202 vollständig schließen).
- Sonstige `ReadScope::from_roots`-Aufrufer (probe-bpf, probe-fs, listener, fsmon, workspace, sentinel, dod-Fassade,
  Tests) bleiben quellkompatibel; betroffen nur durch `..`-Ablehnung in `allows`/`open` (kein Aufrufer gefunden, der
  `..` nutzt).

## 8. Register-IDs

- **F-005**: im Kern geschlossen (Scope-/glob-Mechanik + Regressionstests); produktiv wirksam erst mit W5 D-HW
  (Sentinel-Scopes + Landlock).
- **F-202**: teilweise (Symlink-Klasse jetzt in `harw-dod-cap`/`harw-dod-readfs` getestet); Fixture-/Harness-Seite
  → C-FIXT / W5.

## 9. Offene Annahmen / Restrisiken

- TOCTOU zwischen `canonicalize`/Alias-Prüfung und `File::open(canonical)` bleibt (Befund M in w2-dod-base).
  Für sysfs/procfs kernel-kontrolliert; ein `openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS)`-Pfad bräuchte `libc`/
  `rustix` (Dep-Queue) → nicht umgesetzt.
- Alias-Prüfung setzt voraus, dass `declared` selbst kanonisch ist (auf dem Pi belegt); andernfalls schlägt sie
  sicher fehl (Ablehnung, kein Zulassen).
- Einfache Wurzeln werden weiterhin nicht kanonisiert (Checkout hinter Symlink → leere Fixture-Globs, w2-dod-base §6);
  bewusst nicht geändert.
- Kein Tracing ergänzt: `harw-dod-cap`/`harw-dod-readfs` haben keine `tracing`-Abhängigkeit (keine Dep-Anfrage nötig).
- Keine dep-requests.
