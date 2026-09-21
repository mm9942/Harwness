//! Lesebereich für Sensoren: ein Halbverband, der nur schrumpfen kann.
//!
//! # Verantwortungsbereich
//! [`ReadScope`] modelliert, welche Wurzelverzeichnisse ein Sensor lesen
//! darf. Wie `harw_authority::NetworkScope` (das Vorbild dieser Disziplin) gibt
//! es **kein** `add` und **keine** Vereinigung — nur [`ReadScope::intersection`].
//! Ein Bereich kann auf keinem Weg wachsen; das ist eine Typ-Eigenschaft
//! dieser API, keine Konvention, die ein Aufrufer versehentlich verletzen
//! könnte.
//!
//! Zwei Arten von Wurzeln:
//!
//! - **Einfache Wurzeln** ([`ReadScope::from_roots`]): erlaubt ist, was nach
//!   vollständiger Symlink-Auflösung unter der Wurzel liegt (z. B. `/proc`).
//! - **Alias-Wurzeln** ([`AliasRoot`], [`ReadScope::from_roots_and_aliases`]):
//!   für sysfs-Klassenverzeichnisse wie `/sys/class/thermal`, `/sys/block`
//!   oder `/sys/class/drm`, deren Einträge auf **jedem** Linux Symlinks nach
//!   `/sys/devices/…` sind (Befund F-005; auf dem RPi 5 mit Kernel
//!   6.18.34+rpt-rpi-2712 per `readlink` belegt, z. B.
//!   `/sys/class/thermal/thermal_zone0 -> ../../devices/virtual/thermal/thermal_zone0`).
//!   Eine einfache Wurzel `/sys/class/thermal` lehnt diese Ziele ab, eine
//!   einfache Wurzel `/sys/devices` wäre viel zu weit. Eine Alias-Wurzel
//!   erlaubt stattdessen genau **eine** Symlink-Ebene je Klassen-Eintrag
//!   hinein in ein ausdrücklich benanntes Zielpräfix.
//!
//! # Sicherheitskritischer Pfad
//! [`ReadScope::resolve`] (und darauf aufbauend [`ReadScope::open`]) löst
//! Symlinks **vor** der Bereichsprüfung auf, über
//! `std::path::Path::canonicalize`. Wer zuerst prüft und danach öffnet,
//! prüft nur den *Namen* — das *Ziel* eines Symlinks bleibt bis zum
//! `open()`-Syscall ungeprüft. Deshalb: erst auflösen, dann prüfen, dann den
//! bereits aufgelösten Pfad öffnen.
//!
//! Regeln der Alias-Prüfung (alle Vergleiche über Pfad**komponenten**
//! (`Path::starts_with`), nie über String-Präfixe):
//!
//! 1. Der angefragte Pfad enthält keine `..`-Komponente.
//! 2. Der angefragte Pfad liegt lexikalisch unter `declared`.
//! 3. Ist der Klassen-Eintrag (`declared/<eintrag>`) ein Symlink, wird sein
//!    Ziel genau **eine** Ebene lexikalisch aufgelöst. Dieses Ziel muss unter
//!    `resolved_prefix` liegen **und** mit dem kanonischen Pfad des Eintrags
//!    übereinstimmen — ein Ziel, das selbst wieder ein Symlink ist (doppelter
//!    Symlink) oder über ein symlinktes Zwischenverzeichnis führt, weicht
//!    davon ab und wird abgelehnt.
//! 4. Ist der Eintrag kein Symlink, muss er bereits kanonisch sein.
//! 5. Der kanonische Pfad des **gesamten** angefragten Pfads muss unter
//!    `resolved_prefix` oder `declared` liegen. Tiefere Symlinks (etwa
//!    `card1/device -> ../../../axi:gpu`) sind damit erlaubt, solange sie
//!    innerhalb bleiben; ein Symlink nach außerhalb nie.
//!
//! Direkter Zugriff auf `resolved_prefix` (z. B. `/sys/devices/…` ohne den
//! Umweg über den Klassen-Eintrag) ist **nicht** erlaubt: das Präfix ist nur
//! Ziel-, nie Anfragewurzel.
//!
//! # Verbleibendes Zeitfenster
//! Zwischen `canonicalize` und `File::open` auf dem kanonischen Pfad kann
//! sich eine Komponente dieses Pfads ändern (TOCTOU). Bei sysfs/procfs
//! kontrolliert allein der Kernel die Struktur; für beschreibbare Bäume ist
//! das Fenster nicht geschlossen (siehe Ledger `W3/C-SCOPE`).
//!
//! # Exportierte Typen
//! [`ReadScope`], [`AliasRoot`], [`AliasRootError`] (mit dem vom Derive
//! erzeugten Alias `AliasRootResult<T>`), Konstante [`SYSFS_DEVICES`].
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Daten (`BTreeSet`s) ohne innere Veränderlichkeit:
//! `Send + Sync`, beliebig teilbar. [`ReadScope::resolve`] und
//! [`ReadScope::open`] führen Betriebssystem-I/O aus, halten keine Sperren
//! und sind aus mehreren Threads gleichzeitig aufrufbar.
//!
//! # Fehler
//! [`crate::error::SensorError::OutsideScope`] (Bereichsverletzung; nennt nie
//! das Ziel), [`crate::error::SensorError::Io`] (Canonicalize-/Readlink-/
//! Open-Fehler des Betriebssystems) und [`AliasRootError`] (ungültige
//! Alias-Konfiguration beim Bau).
//!
//! # Examples
//! ```rust
//! use harw_dod_cap::scope::{AliasRoot, ReadScope};
//! use std::path::{Path, PathBuf};
//!
//! let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
//! assert!(scope.allows(Path::new("/proc/stat")));
//! assert!(!scope.allows(Path::new("/etc/passwd")));
//!
//! let thermal = AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal"))?;
//! let scope = ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [thermal]);
//! assert!(scope.allows(Path::new("/sys/class/thermal/thermal_zone0/temp")));
//! assert!(!scope.allows(Path::new("/sys/devices/virtual/thermal/thermal_zone0/temp")));
//! # Ok::<(), harw_dod_cap::scope::AliasRootError>(())
//! ```

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Component, Path, PathBuf};

use harw_macros::HarwError;

use crate::error::SensorError;

/// Kanonisches Zielpräfix aller sysfs-Klassen-Symlinks: `/sys/devices`.
///
/// # Description
/// Belegt auf dem RPi 5 (Kernel 6.18.34+rpt-rpi-2712) für
/// `/sys/class/thermal/*`, `/sys/block/*`, `/sys/class/block/*`,
/// `/sys/class/drm/*` und `/sys/class/net/*` (siehe Ledger `W3/C-SCOPE`).
pub const SYSFS_DEVICES: &str = "/sys/devices";

/// Ungültige Konfiguration einer [`AliasRoot`].
///
/// # Description
/// Entsteht ausschließlich beim Bau über [`AliasRoot::new`] bzw.
/// [`AliasRoot::sysfs_class`], nie während eines Lesezugriffs. Die Varianten
/// nennen den abgelehnten Konfigurationspfad — das ist kein vom Host
/// gelesener Wert, sondern ein Literal aus der Sensor-Konfiguration.
///
/// # Concurrency
/// `Send + Sync`; reine Daten.
#[derive(Debug, HarwError)]
pub enum AliasRootError {
    /// `declared` ist nicht absolut oder enthält `.`/`..`-Komponenten.
    #[msg("alias root declared path '{path}' must be absolute and free of '.' and '..'")]
    DeclaredNotNormalized {
        /// Der abgelehnte Pfad, verlustbehaftet als UTF-8 dargestellt.
        path: String,
    },

    /// `resolved_prefix` ist nicht absolut oder enthält `.`/`..`-Komponenten.
    #[msg("alias root resolved prefix '{path}' must be absolute and free of '.' and '..'")]
    ResolvedPrefixNotNormalized {
        /// Der abgelehnte Pfad, verlustbehaftet als UTF-8 dargestellt.
        path: String,
    },

    /// `declared` oder `resolved_prefix` ist das Dateisystem-Wurzelverzeichnis
    /// `/` — das würde jede Prüfung wirkungslos machen.
    #[msg("alias root {which} must not be the filesystem root '/'")]
    FilesystemRoot {
        /// Welcher Teil betroffen ist: `"declared path"` oder
        /// `"resolved prefix"`.
        which: &'static str,
    },
}

/// Eine Alias-Wurzel: ein Klassenverzeichnis, dessen Einträge genau eine
/// Symlink-Ebene tief in ein benanntes Zielpräfix zeigen dürfen.
///
/// # Description
/// `declared` ist der Name, unter dem ein Sensor anfragt (z. B.
/// `/sys/class/thermal`); `resolved_prefix` ist das Präfix, in dem die
/// kanonischen Ziele liegen müssen (z. B. `/sys/devices`). Beide Pfade sind
/// absolut und komponentenweise normalisiert (keine `.`/`..`, kein `/` als
/// Ganzes); die Invariante stellt der Konstruktor her. Die genauen
/// Prüfregeln stehen in der Modul-Dokumentation.
///
/// # Concurrency
/// `Send + Sync`; reine Daten.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AliasRoot {
    declared: PathBuf,
    resolved_prefix: PathBuf,
}

impl AliasRoot {
    /// Baut eine Alias-Wurzel und prüft beide Pfade.
    ///
    /// # Arguments
    /// - `declared` (`PathBuf`): das Klassenverzeichnis, unter dem angefragt
    ///   wird; absolut, ohne `.`/`..`, nicht `/`.
    /// - `resolved_prefix` (`PathBuf`): das Präfix, unter dem kanonische
    ///   Ziele liegen müssen; absolut, ohne `.`/`..`, nicht `/`.
    ///
    /// # Returns
    /// Die Alias-Wurzel mit komponentenweise normalisierten Pfaden (doppelte
    /// oder abschließende `/` fallen weg).
    ///
    /// # Errors
    /// - [`AliasRootError::DeclaredNotNormalized`] /
    ///   [`AliasRootError::ResolvedPrefixNotNormalized`]: relativer Pfad oder
    ///   `.`/`..`-Komponente.
    /// - [`AliasRootError::FilesystemRoot`]: einer der Pfade ist `/`.
    ///
    /// # Concurrency
    /// Keine I/O; beliebig aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::scope::AliasRoot;
    /// use std::path::{Path, PathBuf};
    ///
    /// let alias = AliasRoot::new(PathBuf::from("/sys/block/"), PathBuf::from("/sys/devices"))?;
    /// assert_eq!(alias.declared(), Path::new("/sys/block"));
    /// assert!(AliasRoot::new(PathBuf::from("sys/block"), PathBuf::from("/sys/devices")).is_err());
    /// # Ok::<(), harw_dod_cap::scope::AliasRootError>(())
    /// ```
    pub fn new(declared: PathBuf, resolved_prefix: PathBuf) -> Result<Self, AliasRootError> {
        let declared = normalized_absolute(&declared).ok_or_else(|| {
            AliasRootError::DeclaredNotNormalized {
                path: declared.to_string_lossy().into_owned(),
            }
        })?;
        let resolved_prefix = normalized_absolute(&resolved_prefix).ok_or_else(|| {
            AliasRootError::ResolvedPrefixNotNormalized {
                path: resolved_prefix.to_string_lossy().into_owned(),
            }
        })?;
        if declared.parent().is_none() {
            return Err(AliasRootError::FilesystemRoot {
                which: "declared path",
            });
        }
        if resolved_prefix.parent().is_none() {
            return Err(AliasRootError::FilesystemRoot {
                which: "resolved prefix",
            });
        }
        Ok(Self {
            declared,
            resolved_prefix,
        })
    }

    /// Baut die übliche sysfs-Alias-Wurzel mit Zielpräfix [`SYSFS_DEVICES`].
    ///
    /// # Arguments
    /// - `declared` (`PathBuf`): ein sysfs-Klassenverzeichnis, z. B.
    ///   `/sys/class/thermal`, `/sys/block` oder `/sys/class/drm`.
    ///
    /// # Returns
    /// `AliasRoot { declared, resolved_prefix: /sys/devices }`.
    ///
    /// # Errors
    /// Wie [`Self::new`] für `declared`.
    ///
    /// # Concurrency
    /// Keine I/O; beliebig aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::scope::AliasRoot;
    /// use std::path::{Path, PathBuf};
    ///
    /// let alias = AliasRoot::sysfs_class(PathBuf::from("/sys/class/drm"))?;
    /// assert_eq!(alias.resolved_prefix(), Path::new("/sys/devices"));
    /// # Ok::<(), harw_dod_cap::scope::AliasRootError>(())
    /// ```
    pub fn sysfs_class(declared: PathBuf) -> Result<Self, AliasRootError> {
        Self::new(declared, PathBuf::from(SYSFS_DEVICES))
    }

    /// Das angefragte Klassenverzeichnis.
    ///
    /// # Returns
    /// Den normalisierten `declared`-Pfad, geliehen.
    #[must_use]
    pub fn declared(&self) -> &Path {
        &self.declared
    }

    /// Das Präfix, unter dem kanonische Ziele liegen müssen.
    ///
    /// # Returns
    /// Den normalisierten `resolved_prefix`-Pfad, geliehen.
    #[must_use]
    pub fn resolved_prefix(&self) -> &Path {
        &self.resolved_prefix
    }

    // Prüft einen bereits kanonisierten Pfad gegen diese Alias-Wurzel
    // (Regeln 2–5 der Modul-Dokumentation). `Ok(false)` heißt „nicht über
    // diesen Alias erlaubt“, `Err` nur bei Betriebssystemfehlern.
    fn admits(&self, path: &Path, canonical: &Path) -> Result<bool, SensorError> {
        let Ok(rest) = path.strip_prefix(&self.declared) else {
            return Ok(false);
        };
        match rest.components().next() {
            None => return Ok(canonical == self.declared),
            Some(Component::Normal(entry)) => {
                let entry_path = self.declared.join(entry);
                let metadata = std::fs::symlink_metadata(&entry_path)?;
                let entry_canonical = entry_path.canonicalize()?;
                if metadata.file_type().is_symlink() {
                    let target = std::fs::read_link(&entry_path)?;
                    let Some(one_level) = lexical_resolve(&self.declared, &target) else {
                        return Ok(false);
                    };
                    if one_level == self.resolved_prefix
                        || !one_level.starts_with(&self.resolved_prefix)
                    {
                        return Ok(false);
                    }
                    // Weicht die vollständige Auflösung von der einen Ebene
                    // ab, war das Ziel selbst ein Symlink oder führte über ein
                    // symlinktes Zwischenverzeichnis: abgelehnt.
                    if entry_canonical != one_level {
                        return Ok(false);
                    }
                } else if entry_canonical != entry_path {
                    // Ein Nicht-Symlink-Eintrag, dessen kanonischer Pfad
                    // abweicht, liegt unter einem symlinkten `declared`.
                    return Ok(false);
                }
            }
            Some(_) => return Ok(false),
        }
        Ok(canonical.starts_with(&self.resolved_prefix) || canonical.starts_with(&self.declared))
    }
}

/// Ein Lesebereich: eine Menge erlaubter einfacher Wurzeln und Alias-Wurzeln.
///
/// # Description
/// Halbverband wie `NetworkScope`: Es gibt [`Self::intersection`] (Meet),
/// aber bewusst kein `add` und keine Vereinigung (Join). Ein einmal gebauter
/// Bereich kann also nur enger werden, nie weiter.
///
/// Einfache Wurzeln werden so gespeichert, wie sie übergeben wurden —
/// [`Self::from_roots`] kanonisiert nicht, weil Kanonisierung eine fehlbare
/// Dateisystemoperation ist und dieser Konstruktor unfehlbar bleibt. Wurzeln
/// sollten deshalb bereits kanonische, symlink-freie Verzeichnisse sein.
/// Alias-Wurzeln ([`AliasRoot`]) sind beim Bau bereits geprüft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadScope {
    roots: BTreeSet<PathBuf>,
    aliases: BTreeSet<AliasRoot>,
}

impl ReadScope {
    /// Baut einen Bereich aus einfachen Wurzelpfaden (ohne Alias-Wurzeln).
    ///
    /// # Arguments
    /// - `roots` (`impl IntoIterator<Item = PathBuf>`): erlaubte
    ///   Wurzelverzeichnisse. Duplikate fallen durch die Mengensemantik weg;
    ///   eine leere Eingabe liefert den leeren Bereich.
    ///
    /// # Returns
    /// Einen `ReadScope`, dessen [`Self::allows`] genau die Pfade akzeptiert,
    /// die unterhalb einer der übergebenen Wurzeln liegen.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::ReadScope;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/sys/class/thermal")]);
    /// assert!(scope.allows(std::path::Path::new("/sys/class/thermal/thermal_zone0")));
    /// ```
    #[must_use]
    pub fn from_roots(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self::from_roots_and_aliases(roots, std::iter::empty())
    }

    /// Baut einen Bereich aus einfachen Wurzeln und Alias-Wurzeln.
    ///
    /// # Description
    /// Ein Konstruktor, keine Erweiterung eines bestehenden Bereichs: die
    /// Halbverband-Eigenschaft (nur [`Self::intersection`] auf fertigen
    /// Bereichen) bleibt unberührt.
    ///
    /// # Arguments
    /// - `roots` (`impl IntoIterator<Item = PathBuf>`): einfache Wurzeln.
    /// - `aliases` (`impl IntoIterator<Item = AliasRoot>`): Alias-Wurzeln für
    ///   sysfs-Klassenverzeichnisse.
    ///
    /// # Returns
    /// Den kombinierten Bereich.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::scope::{AliasRoot, ReadScope};
    /// use std::path::{Path, PathBuf};
    ///
    /// let block = AliasRoot::sysfs_class(PathBuf::from("/sys/block"))?;
    /// let scope = ReadScope::from_roots_and_aliases([PathBuf::from("/proc")], [block]);
    /// let roots: Vec<&Path> = scope.roots().collect();
    /// assert_eq!(roots, [Path::new("/proc"), Path::new("/sys/block")]);
    /// # Ok::<(), harw_dod_cap::scope::AliasRootError>(())
    /// ```
    #[must_use]
    pub fn from_roots_and_aliases(
        roots: impl IntoIterator<Item = PathBuf>,
        aliases: impl IntoIterator<Item = AliasRoot>,
    ) -> Self {
        Self {
            roots: roots.into_iter().collect(),
            aliases: aliases.into_iter().collect(),
        }
    }

    /// Die Anfragewurzeln dieses Bereichs.
    ///
    /// # Description
    /// Ein lesender Zugriff, damit ein Aufrufer, der das Dateisystem
    /// **durchlaufen** muss, weiß, wo er anfangen darf. Liefert zuerst die
    /// einfachen Wurzeln, danach die `declared`-Pfade der Alias-Wurzeln (ohne
    /// Duplikate) — nie ein `resolved_prefix`, denn das ist keine
    /// Anfragewurzel. Ein Bereich aus genau einer Alias-Wurzel liefert also
    /// genau deren `declared`-Pfad; Sensoren, die `roots().next()` als
    /// Glob-Basis nutzen, funktionieren unverändert.
    ///
    /// # Returns
    /// Ein Iterator über die Wurzelpfade, in stabiler Ordnung.
    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        self.roots.iter().map(PathBuf::as_path).chain(
            self.aliases
                .iter()
                .map(AliasRoot::declared)
                .filter(|declared| !self.roots.contains(*declared)),
        )
    }

    /// Die Alias-Wurzeln dieses Bereichs.
    ///
    /// # Returns
    /// Ein Iterator über die [`AliasRoot`]s, in stabiler Ordnung.
    pub fn alias_roots(&self) -> impl Iterator<Item = &AliasRoot> {
        self.aliases.iter()
    }

    /// Schnitt zweier Bereiche. Es gibt bewusst keine Vereinigung.
    ///
    /// # Description
    /// Exakte Mengenoperation getrennt über einfache Wurzeln und über
    /// Alias-Wurzeln (eine Alias-Wurzel bleibt nur, wenn `declared` **und**
    /// `resolved_prefix` in beiden Bereichen identisch sind). Verschachtelte
    /// Wurzeln (z. B. `/a/b` und `/a`) ergeben bewusst einen konservativeren
    /// Schnitt als die theoretisch größtmögliche gemeinsame Menge.
    ///
    /// # Arguments
    /// - `other` (`&ReadScope`): der zweite Bereich.
    ///
    /// # Returns
    /// Einen `ReadScope`, dessen erlaubte Pfadmenge Teilmenge sowohl von
    /// `self` als auch von `other` ist. Kommutativ und idempotent.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::ReadScope;
    /// use std::path::PathBuf;
    ///
    /// let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
    /// let b = ReadScope::from_roots([PathBuf::from("/sys"), PathBuf::from("/etc")]);
    /// let ab = a.intersection(&b);
    ///
    /// assert!(ab.allows(std::path::Path::new("/sys/class/thermal")));
    /// assert!(!ab.allows(std::path::Path::new("/proc/stat")));
    /// ```
    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        Self {
            roots: self.roots.intersection(&other.roots).cloned().collect(),
            aliases: self.aliases.intersection(&other.aliases).cloned().collect(),
        }
    }

    /// Liegt `path` (als Name, ohne Auflösung) im Bereich?
    ///
    /// # Description
    /// Erlaubt ist ein exakter Treffer auf einer einfachen Wurzel oder einem
    /// Alias-`declared`-Pfad oder ein Pfad *unterhalb* davon. Verglichen wird
    /// komponentenweise (`Path::starts_with`): `/srv/data` erlaubt
    /// `/srv/data/x`, aber **nicht** `/srv/database`. Ein Pfad mit einer
    /// `..`-Komponente ist nie erlaubt, weil er lexikalisch unter einer
    /// Wurzel beginnen und trotzdem darüber hinaus zeigen kann.
    ///
    /// Keine Dateisystem-I/O, keine Symlink-Auflösung; die symlink-sichere
    /// Prüfung eines tatsächlichen Zugriffs übernimmt [`Self::resolve`].
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): der zu prüfende Pfad.
    ///
    /// # Returns
    /// `true`, wenn `path` lexikalisch unter einer Anfragewurzel liegt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::ReadScope;
    /// use std::path::{Path, PathBuf};
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
    /// assert!(scope.allows(Path::new("/srv/data/report.json")));
    /// assert!(!scope.allows(Path::new("/srv/database/report.json")));
    /// assert!(!scope.allows(Path::new("/srv/data/../../etc/passwd")));
    /// ```
    #[must_use]
    pub fn allows(&self, path: &Path) -> bool {
        !has_parent_dir(path) && self.roots().any(|root| path.starts_with(root))
    }

    /// Löst `path` auf und prüft das Ergebnis gegen den Bereich.
    ///
    /// # Description
    /// 1. `..`-Komponente im angefragten Pfad → abgelehnt.
    /// 2. `canonicalize` (löst alle Symlinks auf).
    /// 3. Liegt das kanonische Ziel unter einer **einfachen** Wurzel → erlaubt.
    /// 4. Sonst: für jede Alias-Wurzel, unter deren `declared` der angefragte
    ///    Name lexikalisch liegt, die Alias-Regeln der Modul-Dokumentation
    ///    (eine Symlink-Ebene je Klassen-Eintrag, Ziel unter
    ///    `resolved_prefix`, kein doppelter Symlink, kanonisches Gesamtziel
    ///    unter `resolved_prefix` oder `declared`).
    /// 5. Sonst → abgelehnt.
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): der angeforderte Pfad, vor Auflösung.
    ///
    /// # Returns
    /// Den kanonischen, geprüften Pfad (`PathBuf`).
    ///
    /// # Errors
    /// - [`SensorError::OutsideScope`]: Bereichsverletzung (nennt nie das
    ///   Ziel).
    /// - [`SensorError::Io`]: `canonicalize`, `symlink_metadata` oder
    ///   `read_link` scheitert (z. B. Pfad existiert nicht).
    ///
    /// # Concurrency
    /// Zustandslos, keine Sperren; aus mehreren Threads aufrufbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_cap::scope::{AliasRoot, ReadScope};
    /// use std::path::{Path, PathBuf};
    ///
    /// let thermal = AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal"))
    ///     .map_err(|_| harw_dod_cap::SensorError::ToolFault)?;
    /// let scope = ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [thermal]);
    /// let real = scope.resolve(Path::new("/sys/class/thermal/thermal_zone0/temp"))?;
    /// assert!(real.starts_with("/sys/devices"));
    /// # Ok::<(), harw_dod_cap::SensorError>(())
    /// ```
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, SensorError> {
        if has_parent_dir(path) {
            return Err(SensorError::OutsideScope);
        }
        let canonical = path.canonicalize().map_err(SensorError::Io)?;

        if self.roots.iter().any(|root| canonical.starts_with(root)) {
            return Ok(canonical);
        }

        for alias in self
            .aliases
            .iter()
            .filter(|alias| path.starts_with(&alias.declared))
        {
            if alias.admits(path, &canonical)? {
                return Ok(canonical);
            }
        }

        Err(SensorError::OutsideScope)
    }

    /// Öffnet `path` **nach** Auflösung und Prüfung über [`Self::resolve`].
    ///
    /// # Description
    /// Geöffnet wird der bereits aufgelöste und geprüfte Pfad, nicht der
    /// angefragte Name. Zum verbleibenden Zeitfenster siehe
    /// Modul-Dokumentation.
    ///
    /// # Arguments
    /// - `path` (`&std::path::Path`): der angeforderte Pfad, vor der
    ///   Auflösung.
    ///
    /// # Returns
    /// Die geöffnete [`std::fs::File`] auf den aufgelösten Pfad.
    ///
    /// # Errors
    /// - [`SensorError::OutsideScope`]: siehe [`Self::resolve`].
    /// - [`SensorError::Io`]: Auflösung oder Öffnen scheitert.
    ///
    /// # Concurrency
    /// Zustandslos, keine Sperren; aus mehreren Threads aufrufbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_cap::ReadScope;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
    /// let file = scope.open(std::path::Path::new("/proc/stat"))?;
    /// drop(file);
    /// # Ok::<(), harw_dod_cap::error::SensorError>(())
    /// ```
    pub fn open(&self, path: &Path) -> Result<File, SensorError> {
        let canonical = self.resolve(path)?;
        File::open(&canonical).map_err(SensorError::Io)
    }
}

// `true`, wenn `path` irgendwo eine `..`-Komponente enthält.
fn has_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

// Normalisiert einen absoluten Pfad komponentenweise; `None` bei relativem
// Pfad, `.`/`..` oder Windows-Präfix.
fn normalized_absolute(path: &Path) -> Option<PathBuf> {
    let mut components = path.components();
    if components.next() != Some(Component::RootDir) {
        return None;
    }
    let mut out = PathBuf::from("/");
    for component in components {
        match component {
            Component::Normal(name) => out.push(name),
            Component::RootDir
            | Component::CurDir
            | Component::ParentDir
            | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

// Löst ein Symlink-Ziel genau eine Ebene lexikalisch auf: relativ zu `base`
// (dem Verzeichnis, das den Symlink enthält), `..` entfernt die letzte
// Komponente. `None`, wenn `..` über `/` hinaus zeigen würde oder ein
// Windows-Präfix auftaucht.
fn lexical_resolve(base: &Path, target: &Path) -> Option<PathBuf> {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::Normal(name) => out.push(name),
            Component::Prefix(_) => return None,
        }
    }
    if out.is_absolute() { Some(out) } else { None }
}

#[cfg(test)]
mod tests {
    use super::{AliasRoot, AliasRootError, ReadScope, lexical_resolve};
    use crate::error::SensorError;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_intersection_is_commutative() {
        let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
        let b = ReadScope::from_roots([PathBuf::from("/sys"), PathBuf::from("/etc")]);
        assert_eq!(a.intersection(&b), b.intersection(&a));
    }

    #[test]
    fn test_intersection_is_idempotent() {
        let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
        assert_eq!(a.intersection(&a), a);
    }

    #[test]
    fn test_intersection_result_is_subset_of_both_inputs() {
        let a = ReadScope::from_roots([PathBuf::from("/proc"), PathBuf::from("/sys")]);
        let b = ReadScope::from_roots([PathBuf::from("/sys"), PathBuf::from("/etc")]);
        let ab = a.intersection(&b);

        assert!(ab.allows(Path::new("/sys/class/thermal")));
        assert!(a.allows(Path::new("/sys/class/thermal")));
        assert!(b.allows(Path::new("/sys/class/thermal")));

        assert!(!ab.allows(Path::new("/proc/stat")));
        assert!(!ab.allows(Path::new("/etc/passwd")));
    }

    #[test]
    fn test_intersection_keeps_only_identical_alias_roots() {
        let thermal = AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal"))
            .expect("valid alias root");
        let other_prefix = AliasRoot::new(
            PathBuf::from("/sys/class/thermal"),
            PathBuf::from("/sys/devices/virtual"),
        )
        .expect("valid alias root");
        let block = AliasRoot::sysfs_class(PathBuf::from("/sys/block")).expect("valid alias root");

        let a =
            ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [thermal.clone(), block]);
        let b = ReadScope::from_roots_and_aliases(
            Vec::<PathBuf>::new(),
            [thermal.clone(), other_prefix],
        );
        let ab = a.intersection(&b);

        assert_eq!(ab.alias_roots().collect::<Vec<_>>(), vec![&thermal]);
        assert!(!ab.allows(Path::new("/sys/block/mmcblk0/stat")));
    }

    #[test]
    fn test_allows_path_in_scope() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(scope.allows(Path::new("/srv/data/report.json")));
    }

    #[test]
    fn test_allows_path_outside_scope() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(!scope.allows(Path::new("/etc/passwd")));
    }

    #[test]
    fn test_allows_rejects_prefix_without_directory_boundary() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(!scope.allows(Path::new("/srv/database/report.json")));
        assert!(!scope.allows(Path::new("/srv/database")));
    }

    #[test]
    fn test_allows_exact_root_match() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(scope.allows(Path::new("/srv/data")));
    }

    #[test]
    fn test_allows_rejects_parent_dir_component() {
        let scope = ReadScope::from_roots([PathBuf::from("/srv/data")]);
        assert!(!scope.allows(Path::new("/srv/data/../../etc/passwd")));
    }

    #[test]
    fn test_allows_alias_declared_but_not_resolved_prefix() {
        let alias =
            AliasRoot::sysfs_class(PathBuf::from("/sys/class/drm")).expect("valid alias root");
        let scope = ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [alias]);
        assert!(scope.allows(Path::new("/sys/class/drm/card1/device")));
        assert!(!scope.allows(Path::new("/sys/devices/platform/axi/axi:gpu")));
    }

    #[test]
    fn test_roots_lists_plain_roots_then_alias_declared_without_duplicates() {
        let block = AliasRoot::sysfs_class(PathBuf::from("/sys/block")).expect("valid alias root");
        let proc_alias =
            AliasRoot::new(PathBuf::from("/proc"), PathBuf::from("/proc")).expect("valid alias");
        let scope =
            ReadScope::from_roots_and_aliases([PathBuf::from("/proc")], [block, proc_alias]);
        assert_eq!(
            scope.roots().collect::<Vec<_>>(),
            vec![Path::new("/proc"), Path::new("/sys/block")]
        );
    }

    #[test]
    fn test_alias_root_new_normalizes_trailing_and_double_slashes() {
        let alias = AliasRoot::new(PathBuf::from("/sys//block/"), PathBuf::from("/sys/devices/"))
            .expect("valid alias root");
        assert_eq!(alias.declared(), Path::new("/sys/block"));
        assert_eq!(alias.resolved_prefix(), Path::new("/sys/devices"));
    }

    #[test]
    fn test_alias_root_new_rejects_relative_declared() {
        let err = AliasRoot::new(PathBuf::from("sys/block"), PathBuf::from("/sys/devices"))
            .expect_err("relative declared path must be rejected");
        assert!(matches!(err, AliasRootError::DeclaredNotNormalized { .. }));
    }

    #[test]
    fn test_alias_root_new_rejects_parent_dir_in_prefix() {
        let err = AliasRoot::new(PathBuf::from("/sys/block"), PathBuf::from("/sys/devices/../.."))
            .expect_err("'..' in resolved prefix must be rejected");
        assert!(matches!(err, AliasRootError::ResolvedPrefixNotNormalized { .. }));
    }

    #[test]
    fn test_alias_root_new_rejects_filesystem_root_prefix() {
        let err = AliasRoot::new(PathBuf::from("/sys/block"), PathBuf::from("/"))
            .expect_err("'/' as resolved prefix must be rejected");
        assert!(matches!(
            err,
            AliasRootError::FilesystemRoot { which: "resolved prefix" }
        ));
        assert_eq!(
            err.to_string(),
            "alias root resolved prefix must not be the filesystem root '/'"
        );
    }

    #[test]
    fn test_alias_root_sysfs_class_uses_sys_devices() {
        let alias =
            AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal")).expect("valid alias root");
        assert_eq!(alias.resolved_prefix(), Path::new("/sys/devices"));
    }

    #[test]
    fn test_lexical_resolve_relative_sysfs_target() {
        let resolved = lexical_resolve(
            Path::new("/sys/class/thermal"),
            Path::new("../../devices/virtual/thermal/thermal_zone0"),
        );
        assert_eq!(
            resolved,
            Some(PathBuf::from("/sys/devices/virtual/thermal/thermal_zone0"))
        );
    }

    #[test]
    fn test_lexical_resolve_rejects_escape_above_root() {
        assert_eq!(lexical_resolve(Path::new("/sys"), Path::new("../../../x")), None);
    }

    #[test]
    fn test_open_reads_file_inside_scope() {
        use std::io::Read;

        let dir = tempfile::tempdir().expect("tempdir for scope root");
        let file_path = dir.path().join("data.txt");
        std::fs::write(&file_path, b"hello").expect("write fixture file");

        let scope = ReadScope::from_roots([dir.path().to_path_buf()]);
        let mut file = scope
            .open(&file_path)
            .expect("open must succeed for a file inside the scope");

        let mut contents = String::new();
        file.read_to_string(&mut contents).expect("read fixture file");
        assert_eq!(contents, "hello");
    }

    #[cfg(unix)]
    #[test]
    fn test_open_symlink_outside_scope_returns_outside_scope_without_naming_target() {
        use std::os::unix::fs::symlink;

        let inside = tempfile::tempdir().expect("tempdir as scope root");
        let outside = tempfile::tempdir().expect("tempdir outside the scope");
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, b"top secret").expect("write secret fixture file");

        let link = inside.path().join("link-to-secret");
        symlink(&secret, &link).expect("create symlink pointing outside the scope");

        let scope = ReadScope::from_roots([inside.path().to_path_buf()]);
        let err = scope
            .open(&link)
            .expect_err("a symlink resolving outside the scope must be rejected");

        assert!(matches!(err, SensorError::OutsideScope));

        let message = err.to_string();
        assert_eq!(message, "path resolves outside the sensor read scope");
        let outside_display = outside.path().to_string_lossy().into_owned();
        let secret_display = secret.to_string_lossy().into_owned();
        assert!(!message.contains("secret"));
        assert!(!message.contains(outside_display.as_str()));
        assert!(!message.contains(secret_display.as_str()));
    }

    /// Nachgebauter sysfs-Baum mit echten Symlinks, strukturgleich zum RPi 5
    /// (`/sys/class/thermal/thermal_zone0 -> ../../devices/virtual/thermal/thermal_zone0`,
    /// `/sys/class/drm/card1/device -> ../../../axi:gpu`).
    #[cfg(unix)]
    struct SysTree {
        _dir: tempfile::TempDir,
        base: PathBuf,
    }

    #[cfg(unix)]
    impl SysTree {
        fn new() -> Self {
            use std::fs;
            use std::os::unix::fs::symlink;

            let dir = tempfile::tempdir().expect("tempdir for fake sysfs");
            // Kanonisieren, damit ein symlinktes Temp-Verzeichnis die
            // Alias-Regel 4 nicht verfälscht.
            let base = dir.path().canonicalize().expect("canonical tempdir");
            let sys = base.join("sys");

            let zone0 = sys.join("devices/virtual/thermal/thermal_zone0");
            fs::create_dir_all(&zone0).expect("create zone0");
            fs::write(zone0.join("temp"), "42000\n").expect("write temp");

            let class = sys.join("class/thermal");
            fs::create_dir_all(&class).expect("create class dir");
            symlink("../../devices/virtual/thermal/thermal_zone0", class.join("thermal_zone0"))
                .expect("class -> devices symlink");

            let gpu = sys.join("devices/platform/axi/axi:gpu");
            let card1 = gpu.join("drm/card1");
            fs::create_dir_all(&card1).expect("create card1");
            fs::write(gpu.join("gpu_busy_percent"), "7\n").expect("write busy");
            symlink("../../../axi:gpu", card1.join("device")).expect("device symlink");
            let drm = sys.join("class/drm");
            fs::create_dir_all(&drm).expect("create drm class dir");
            symlink("../../devices/platform/axi/axi:gpu/drm/card1", drm.join("card1"))
                .expect("drm class symlink");
            fs::write(drm.join("version"), "drm 1.1.0\n").expect("write version");

            let outside = base.join("outside");
            fs::create_dir_all(&outside).expect("create outside");
            fs::write(outside.join("secret"), "geheim\n").expect("write secret");

            Self { _dir: dir, base }
        }

        fn sys(&self, rel: &str) -> PathBuf {
            self.base.join("sys").join(rel)
        }

        fn thermal_scope(&self) -> ReadScope {
            let alias = AliasRoot::new(self.sys("class/thermal"), self.sys("devices"))
                .expect("valid alias root");
            ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [alias])
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_plain_class_root_rejects_sysfs_symlink_regression_f005() {
        let tree = SysTree::new();
        let scope = ReadScope::from_roots([tree.sys("class/thermal")]);
        let err = scope
            .resolve(&tree.sys("class/thermal/thermal_zone0/temp"))
            .expect_err("plain class root must not follow into /sys/devices");
        assert!(matches!(err, SensorError::OutsideScope));
    }

    #[cfg(unix)]
    #[test]
    fn test_open_alias_class_entry_into_resolved_prefix_reads_value() {
        use std::io::Read;

        let tree = SysTree::new();
        let scope = tree.thermal_scope();
        let requested = tree.sys("class/thermal/thermal_zone0/temp");

        let resolved = scope.resolve(&requested).expect("alias must admit class entry");
        assert_eq!(resolved, tree.sys("devices/virtual/thermal/thermal_zone0/temp"));

        let mut content = String::new();
        scope
            .open(&requested)
            .expect("open via alias")
            .read_to_string(&mut content)
            .expect("read temp");
        assert_eq!(content, "42000\n");
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_deeper_symlink_inside_prefix_is_admitted() {
        let tree = SysTree::new();
        let alias = AliasRoot::new(tree.sys("class/drm"), tree.sys("devices"))
            .expect("valid alias root");
        let scope = ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [alias]);

        let resolved = scope
            .resolve(&tree.sys("class/drm/card1/device/gpu_busy_percent"))
            .expect("card1/device stays inside /sys/devices");
        assert_eq!(resolved, tree.sys("devices/platform/axi/axi:gpu/gpu_busy_percent"));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_regular_class_entry_is_admitted() {
        let tree = SysTree::new();
        let alias = AliasRoot::new(tree.sys("class/drm"), tree.sys("devices"))
            .expect("valid alias root");
        let scope = ReadScope::from_roots_and_aliases(Vec::<PathBuf>::new(), [alias]);

        let resolved = scope
            .resolve(&tree.sys("class/drm/version"))
            .expect("regular file under declared is admitted");
        assert_eq!(resolved, tree.sys("class/drm/version"));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_pointing_outside_prefix_is_rejected() {
        use std::os::unix::fs::symlink;

        let tree = SysTree::new();
        symlink("../../../outside", tree.sys("class/thermal/thermal_zone9"))
            .expect("evil class symlink");
        let scope = tree.thermal_scope();

        let err = scope
            .resolve(&tree.sys("class/thermal/thermal_zone9/secret"))
            .expect_err("alias target outside resolved prefix must be rejected");
        assert!(matches!(err, SensorError::OutsideScope));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_double_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let tree = SysTree::new();
        // devices/hop -> virtual/thermal/thermal_zone0 (zweite Ebene), und
        // class/thermal/thermal_zone1 -> ../../devices/hop (erste Ebene).
        symlink("virtual/thermal/thermal_zone0", tree.sys("devices/hop"))
            .expect("second-level symlink");
        symlink("../../devices/hop", tree.sys("class/thermal/thermal_zone1"))
            .expect("first-level symlink");
        let scope = tree.thermal_scope();

        let err = scope
            .resolve(&tree.sys("class/thermal/thermal_zone1/temp"))
            .expect_err("symlink to symlink must be rejected even if the end is inside");
        assert!(matches!(err, SensorError::OutsideScope));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_target_through_symlinked_directory_outside_is_rejected() {
        use std::os::unix::fs::symlink;

        let tree = SysTree::new();
        symlink(tree.base.join("outside"), tree.sys("devices/jump")).expect("jump dir symlink");
        symlink("../../devices/jump", tree.sys("class/thermal/thermal_zone2"))
            .expect("class symlink via jump");
        let scope = tree.thermal_scope();

        let err = scope
            .resolve(&tree.sys("class/thermal/thermal_zone2/secret"))
            .expect_err("target via symlinked directory must be rejected");
        assert!(matches!(err, SensorError::OutsideScope));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_deep_symlink_outside_is_rejected() {
        use std::os::unix::fs::symlink;

        let tree = SysTree::new();
        symlink(
            tree.base.join("outside/secret"),
            tree.sys("devices/virtual/thermal/thermal_zone0/leak"),
        )
        .expect("deep evil symlink");
        let scope = tree.thermal_scope();

        let err = scope
            .resolve(&tree.sys("class/thermal/thermal_zone0/leak"))
            .expect_err("deep symlink outside must be rejected");
        assert!(matches!(err, SensorError::OutsideScope));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_parent_dir_component_is_rejected() {
        let tree = SysTree::new();
        let scope = tree.thermal_scope();

        let err = scope
            .resolve(&tree.sys("class/thermal/thermal_zone0/../../../../outside/secret"))
            .expect_err("'..' must be rejected before any resolution");
        assert!(matches!(err, SensorError::OutsideScope));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_direct_resolved_prefix_path_is_rejected() {
        let tree = SysTree::new();
        let scope = tree.thermal_scope();

        let err = scope
            .resolve(&tree.sys("devices/virtual/thermal/thermal_zone0/temp"))
            .expect_err("resolved prefix is not a request root");
        assert!(matches!(err, SensorError::OutsideScope));
    }

    #[cfg(unix)]
    #[test]
    fn test_resolve_alias_missing_entry_returns_io() {
        let tree = SysTree::new();
        let scope = tree.thermal_scope();

        let err = scope
            .resolve(&tree.sys("class/thermal/thermal_zone7/temp"))
            .expect_err("missing entry must fail");
        assert!(
            matches!(err, SensorError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound)
        );
    }
}
