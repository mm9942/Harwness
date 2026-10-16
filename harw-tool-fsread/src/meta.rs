//! Metadaten-Darstellung: `stat`-Felder, Rechte-Strings, Zeit- und Größenformate.
//!
//! # Verantwortung
//! [`Meta`] kapselt ein `rustix::fs::Stat` plattformunabhängig (die Feldtypen
//! von `Stat` unterscheiden sich je Architektur); dazu die Formatierer
//! [`perm_string`], [`iso_utc`] und [`human_size`]. Reine Funktionen, kein
//! I/O, `Send + Sync`.

use rustix::fs::{FileType, Stat};
use serde_json::{Value, json};

/// Art eines Dateisystemeintrags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Reguläre Datei.
    File,
    /// Verzeichnis.
    Dir,
    /// Symbolischer Link.
    Symlink,
    /// FIFO.
    Fifo,
    /// Unix-Socket.
    Socket,
    /// Zeichengerät.
    CharDevice,
    /// Blockgerät.
    BlockDevice,
    /// Unbekannt.
    Unknown,
}

impl Kind {
    /// Langname (`file`, `dir`, `symlink`, …).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Dir => "dir",
            Self::Symlink => "symlink",
            Self::Fifo => "fifo",
            Self::Socket => "socket",
            Self::CharDevice => "char_device",
            Self::BlockDevice => "block_device",
            Self::Unknown => "unknown",
        }
    }

    /// Typzeichen wie in `ls -l` (`-`, `d`, `l`, `p`, `s`, `c`, `b`, `?`).
    #[must_use]
    pub fn type_char(self) -> char {
        match self {
            Self::File => '-',
            Self::Dir => 'd',
            Self::Symlink => 'l',
            Self::Fifo => 'p',
            Self::Socket => 's',
            Self::CharDevice => 'c',
            Self::BlockDevice => 'b',
            Self::Unknown => '?',
        }
    }
}

/// Zeitstempel (Sekunden und Nanosekunden seit der Unix-Epoche).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Ts {
    /// Sekunden seit 1970-01-01T00:00:00Z (kann negativ sein).
    pub secs: i64,
    /// Nanosekunden-Anteil (0..1e9).
    pub nanos: u32,
}

/// Plattformunabhängige Sicht auf ein `stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// Art des Eintrags.
    pub kind: Kind,
    /// `st_size` (Länge des Link-Ziels bei Symlinks).
    pub size: u64,
    /// Rechte-Bits inkl. setuid/setgid/sticky (`& 0o7777`).
    pub mode: u32,
    /// Besitzer.
    pub uid: u32,
    /// Gruppe.
    pub gid: u32,
    /// Anzahl harter Links.
    pub nlink: u64,
    /// Inode-Nummer.
    pub ino: u64,
    /// Geräte-ID des Dateisystems.
    pub dev: u64,
    /// Geräte-ID (bei Gerätedateien).
    pub rdev: u64,
    /// Belegte 512-Byte-Blöcke.
    pub blocks: u64,
    /// Bevorzugte I/O-Blockgröße.
    pub blksize: u64,
    /// Letzter Zugriff.
    pub atime: Ts,
    /// Letzte Änderung des Inhalts.
    pub mtime: Ts,
    /// Letzte Änderung der Metadaten.
    pub ctime: Ts,
}

/// Wandelt einen beliebigen Ganzzahltyp verlustfrei nach `u64` (sonst 0).
fn to_u64<T: TryInto<u64>>(value: T) -> u64 {
    value.try_into().unwrap_or(0)
}

/// Wandelt einen beliebigen Ganzzahltyp nach `i64` (sonst 0).
fn to_i64<T: TryInto<i64>>(value: T) -> i64 {
    value.try_into().unwrap_or(0)
}

/// Wandelt Nanosekunden nach `u32` (sonst 0).
fn to_u32<T: TryInto<u32>>(value: T) -> u32 {
    value.try_into().unwrap_or(0)
}

impl Meta {
    /// Baut die Sicht aus einem `Stat`.
    #[must_use]
    pub fn from_stat(stat: &Stat) -> Self {
        let kind = match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Dir,
            FileType::Symlink => Kind::Symlink,
            FileType::Fifo => Kind::Fifo,
            FileType::Socket => Kind::Socket,
            FileType::CharacterDevice => Kind::CharDevice,
            FileType::BlockDevice => Kind::BlockDevice,
            _ => Kind::Unknown,
        };
        Self {
            kind,
            size: to_u64(stat.st_size),
            mode: to_u32(stat.st_mode) & 0o7777,
            uid: stat.st_uid,
            gid: stat.st_gid,
            nlink: to_u64(stat.st_nlink),
            ino: to_u64(stat.st_ino),
            dev: to_u64(stat.st_dev),
            rdev: to_u64(stat.st_rdev),
            blocks: to_u64(stat.st_blocks),
            blksize: to_u64(stat.st_blksize),
            atime: Ts {
                secs: to_i64(stat.st_atime),
                nanos: to_u32(stat.st_atime_nsec),
            },
            mtime: Ts {
                secs: to_i64(stat.st_mtime),
                nanos: to_u32(stat.st_mtime_nsec),
            },
            ctime: Ts {
                secs: to_i64(stat.st_ctime),
                nanos: to_u32(stat.st_ctime_nsec),
            },
        }
    }

    /// Belegte Bytes auf dem Datenträger (`st_blocks * 512`).
    #[must_use]
    pub fn disk_bytes(&self) -> u64 {
        self.blocks.saturating_mul(512)
    }

    /// `ls -l`-artiger Rechte-String (`drwxr-xr-x`).
    #[must_use]
    pub fn mode_string(&self) -> String {
        let mut out = String::with_capacity(10);
        out.push(self.kind.type_char());
        out.push_str(&perm_string(self.mode));
        out
    }
}

/// Neun Rechtezeichen (`rwxr-xr-x`, mit `s`/`S`/`t`/`T` für Spezialbits).
#[must_use]
pub fn perm_string(mode: u32) -> String {
    let bit = |mask: u32| mode & mask != 0;
    let triple = |r: u32, w: u32, x: u32, special: u32, on: char, off: char| {
        let exec = match (bit(x), bit(special)) {
            (true, true) => on,
            (false, true) => off,
            (true, false) => 'x',
            (false, false) => '-',
        };
        [
            if bit(r) { 'r' } else { '-' },
            if bit(w) { 'w' } else { '-' },
            exec,
        ]
    };
    let mut out = String::with_capacity(9);
    out.extend(triple(0o400, 0o200, 0o100, 0o4000, 's', 'S'));
    out.extend(triple(0o040, 0o020, 0o010, 0o2000, 's', 'S'));
    out.extend(triple(0o004, 0o002, 0o001, 0o1000, 't', 'T'));
    out
}

/// Tage seit 1970-01-01 → (Jahr, Monat, Tag) im proleptischen gregorianischen
/// Kalender (Algorithmus von Howard Hinnant, `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Zerlegte UTC-Zeit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    /// Jahr.
    pub year: i64,
    /// Monat 1..=12.
    pub month: u32,
    /// Tag 1..=31.
    pub day: u32,
    /// Stunde 0..=23.
    pub hour: u32,
    /// Minute 0..=59.
    pub minute: u32,
    /// Sekunde 0..=59.
    pub second: u32,
    /// Wochentag 0 = Sonntag.
    pub weekday: u32,
    /// Tag im Jahr 1..=366.
    pub yday: u32,
}

/// Zerlegt Unix-Sekunden in UTC-Bestandteile.
#[must_use]
pub fn civil_utc(secs: i64) -> Civil {
    let days = secs.div_euclid(86_400);
    let rem = u32::try_from(secs.rem_euclid(86_400)).unwrap_or(0);
    let (year, month, day) = civil_from_days(days);
    let weekday = u32::try_from((days + 4).rem_euclid(7)).unwrap_or(0);
    let jan1 = days_from_civil(year, 1, 1);
    let yday = u32::try_from(days - jan1 + 1).unwrap_or(1);
    Civil {
        year,
        month,
        day,
        hour: rem / 3600,
        minute: rem % 3600 / 60,
        second: rem % 60,
        weekday,
        yday,
    }
}

/// (Jahr, Monat, Tag) → Tage seit 1970-01-01 (Gegenstück zu [`civil_from_days`]).
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(month);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-05T12:34:56Z` für Unix-Sekunden.
#[must_use]
pub fn iso_utc(secs: i64) -> String {
    let c = civil_utc(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second
    )
}

/// Menschenlesbare Größe nach `ls -h` (1024er-Stufen, aufgerundet).
#[must_use]
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["", "K", "M", "G", "T", "P"];
    if bytes < 1024 {
        return bytes.to_string();
    }
    let mut unit = 0usize;
    let mut scaled = bytes;
    let mut remainder = 0u64;
    while scaled >= 1024 && unit < UNITS.len() - 1 {
        remainder = scaled % 1024;
        scaled /= 1024;
        unit += 1;
    }
    if scaled < 10 {
        // Eine Nachkommastelle, aufgerundet (wie GNU `ls -h`).
        let tenths = (remainder * 10).div_ceil(1024);
        let (whole, tenths) = if tenths >= 10 {
            (scaled + 1, 0)
        } else {
            (scaled, tenths)
        };
        format!("{whole}.{tenths}{}", UNITS[unit])
    } else {
        let rounded = if remainder > 0 { scaled + 1 } else { scaled };
        format!("{rounded}{}", UNITS[unit])
    }
}

impl Ts {
    /// ISO-8601-UTC-Text mit Sekundengenauigkeit.
    #[must_use]
    pub fn iso(self) -> String {
        iso_utc(self.secs)
    }

    /// JSON-Form `{iso, epoch}`.
    #[must_use]
    pub fn json(self) -> Value {
        json!({ "iso": self.iso(), "epoch": self.secs })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn perm_string_covers_special_bits() -> TestResult {
        assert_eq!(perm_string(0o755), "rwxr-xr-x");
        assert_eq!(perm_string(0o644), "rw-r--r--");
        assert_eq!(perm_string(0o4755), "rwsr-xr-x");
        assert_eq!(perm_string(0o4644), "rwSr--r--");
        assert_eq!(perm_string(0o1777), "rwxrwxrwt");
        assert_eq!(perm_string(0o1776), "rwxrwxrwT");
        assert_eq!(perm_string(0o2755), "rwxr-sr-x");
        Ok(())
    }

    #[test]
    fn iso_utc_known_dates() -> TestResult {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(86_399), "1970-01-01T23:59:59Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_791_200_096), "2026-10-05T11:34:56Z");
        assert_eq!(iso_utc(-1), "1969-12-31T23:59:59Z");
        Ok(())
    }

    #[test]
    fn civil_roundtrip_and_weekday() -> TestResult {
        let c = civil_utc(1_791_200_096);
        assert_eq!((c.year, c.month, c.day), (2026, 10, 5));
        // 2026-10-05 ist ein Montag.
        assert_eq!(c.weekday, 1);
        assert_eq!(c.yday, 278);
        assert_eq!(days_from_civil(2026, 10, 5) * 86_400, 1_791_158_400);
        Ok(())
    }

    #[test]
    fn human_size_matches_ls_h() -> TestResult {
        assert_eq!(human_size(0), "0");
        assert_eq!(human_size(1023), "1023");
        assert_eq!(human_size(1024), "1.0K");
        assert_eq!(human_size(1025), "1.1K");
        assert_eq!(human_size(10 * 1024), "10K");
        assert_eq!(human_size(10 * 1024 + 1), "11K");
        assert_eq!(human_size(1024 * 1024), "1.0M");
        assert!(human_size(u64::MAX).ends_with('P'));
        Ok(())
    }
}
