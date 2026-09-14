//! Relative Zeitangaben ("vor 5 min") für den Session-Picker.
//!
//! Spec-Quelle: Slice A6 des Kontraktdokuments
//! `~/.claude/workspace/coding/planning/harw-scopes-contract.md`
//! ("Schritt 7" / Session-Picker).
//!
//! # Verantwortung
//! Reine Umrechnung eines Zeitpunkts in eine kurze, menschenlesbare
//! deutschsprachige Relativangabe. Kein I/O, kein Zustand.
//!
//! # Zeittyp-Entscheidung
//! Die Spec verlangt primär `jiff::Timestamp`. `harw-tui/Cargo.toml` hat
//! `jiff` jedoch (noch) nicht als Abhängigkeit deklariert, und dieses Modul
//! darf laut Auftrag ausschließlich `relative_time.rs`, `session_picker.rs`
//! und `lib.rs` (nur Modul-Deklaration) verändern — nicht `Cargo.toml`.
//! Deshalb nutzt dieses Modul `std::time::SystemTime` gemäß der im Auftrag
//! vorgesehenen Ausweichoption. Die Differenzberechnung erfolgt in UTC-
//! Sekunden seit `UNIX_EPOCH`; eine lokale Zeitzone wird bewusst nicht
//! berücksichtigt (im Auftrag als "UTC ok, document" freigegeben).
//!
//! # Nebenläufigkeit
//! [`relative_time`] ist eine reine Funktion ohne inneren Zustand und daher
//! aus beliebigen Threads aufrufbar.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Sekundenschwelle, unterhalb derer "gerade eben" statt einer Minutenangabe
/// gezeigt wird.
const JUST_NOW_THRESHOLD_SECS: u64 = 45;

/// Sekunden pro Minute/Stunde/Tag als benannte Konstanten für Lesbarkeit.
const SECS_PER_MINUTE: u64 = 60;
const SECS_PER_HOUR: u64 = 60 * SECS_PER_MINUTE;
const SECS_PER_DAY: u64 = 24 * SECS_PER_HOUR;
const SECS_PER_WEEK: u64 = 7 * SECS_PER_DAY;

/// Formt einen Zeitpunkt `then` relativ zu `now` in eine kurze deutsche
/// Zeitangabe um.
///
/// # Beschreibung
/// Buckets (Spec-Abschnitt Session-Picker):
/// - `< 45s` seit `then` → `"gerade eben"`
/// - `< 60min` → `"vor N min"`
/// - `< 24h` → `"vor N h"`
/// - `< 7d` → `"vor N d"`
/// - sonst → `"dd.mm.yyyy"` (UTC-Kalenderdatum, keine Zeitzonen-Umrechnung)
///
/// Liegt `then` in der Zukunft (nach `now`), wird ebenfalls `"gerade eben"`
/// zurückgegeben, da eine negative Differenz keine sinnvolle Vergangenheits-
/// angabe ergibt.
///
/// # Argumente
/// - `then` (`SystemTime`): der zu beschreibende Zeitpunkt in der Vergangenheit.
/// - `now` (`SystemTime`): der Referenzzeitpunkt ("jetzt").
///
/// # Rückgabe
/// Kurze, deutschsprachige Zeitangabe als `String`.
///
/// # Beispiele
/// ```
/// use std::time::{Duration, SystemTime};
/// use harw_tui::relative_time::relative_time;
///
/// let now = SystemTime::now();
/// let five_min_ago = now - Duration::from_secs(5 * 60);
/// assert_eq!(relative_time(five_min_ago, now), "vor 5 min");
/// ```
pub fn relative_time(then: SystemTime, now: SystemTime) -> String {
    let elapsed = match now.duration_since(then) {
        Ok(duration) => duration,
        // `then` liegt nach `now` (Zukunft) — Zeitspringen/Uhrdrift werden
        // hier absichtlich wie "gerade eben" behandelt statt als Fehler.
        Err(_) => return "gerade eben".to_owned(),
    };

    let secs = elapsed.as_secs();

    if secs < JUST_NOW_THRESHOLD_SECS {
        "gerade eben".to_owned()
    } else if secs < SECS_PER_HOUR {
        let minutes = secs / SECS_PER_MINUTE;
        format!("vor {minutes} min")
    } else if secs < SECS_PER_DAY {
        let hours = secs / SECS_PER_HOUR;
        format!("vor {hours} h")
    } else if secs < SECS_PER_WEEK {
        let days = secs / SECS_PER_DAY;
        format!("vor {days} d")
    } else {
        format_utc_date(then)
    }
}

/// Formt einen Zeitpunkt als UTC-Kalenderdatum `dd.mm.yyyy`.
///
/// # Beschreibung
/// Rechnet die seit `UNIX_EPOCH` vergangene Sekundenzahl in Kalendertage um
/// und wendet Howard Hinnants proleptisch-gregorianischen `civil_from_days`-
/// Algorithmus an, um ohne Zeitzonen-Crate (Cargo.toml dieser Crate hat
/// weder `jiff` noch `chrono`/`time` als Laufzeitabhängigkeit) ein
/// Kalenderdatum zu erhalten. Zeitpunkte vor `UNIX_EPOCH` werden auf den
/// Epoch selbst geklemmt (Randfall, in der Praxis für Session-Zeitstempel
/// irrelevant).
///
/// # Argumente
/// - `at` (`SystemTime`): der zu formatierende Zeitpunkt.
///
/// # Rückgabe
/// Datum als `"dd.mm.yyyy"`-`String`, UTC.
fn format_utc_date(at: SystemTime) -> String {
    let secs_since_epoch = at
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let days_since_epoch = (secs_since_epoch / SECS_PER_DAY) as i64;
    let (year, month, day) = civil_from_days(days_since_epoch);
    format!("{day:02}.{month:02}.{year:04}")
}

/// Howard Hinnants `civil_from_days`: rechnet Tage seit `1970-01-01` (UTC) in
/// ein proleptisch-gregorianisches Kalenderdatum `(Jahr, Monat, Tag)` um.
///
/// # Beschreibung
/// Referenz: "chrono-Compatible Low-Level Date Algorithms" (H. Hinnant).
/// Der Algorithmus ist für den gesamten `i64`-Bereich korrekt und benötigt
/// keine Gleitkommaarithmetik. Hier ausschließlich für positive Tageszahlen
/// (Zeitpunkte ab 1970) verwendet.
///
/// # Argumente
/// - `days` (`i64`): Anzahl Tage seit `1970-01-01`, kann negativ sein.
///
/// # Rückgabe
/// `(year, month, day)` mit `month` in `1..=12` und `day` in `1..=31`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs_from_epoch: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs_from_epoch)
    }

    /// Innerhalb von 45s gilt "gerade eben", exakt darunter.
    #[test]
    fn test_just_now_below_threshold() {
        let now = at(1_000_000);
        let then = now - Duration::from_secs(44);
        assert_eq!(relative_time(then, now), "gerade eben");
    }

    /// Bei exakt 0 Sekunden Differenz ebenfalls "gerade eben".
    #[test]
    fn test_just_now_zero_diff() {
        let now = at(1_000_000);
        assert_eq!(relative_time(now, now), "gerade eben");
    }

    /// Bei exakt 45s wechselt der Bucket zu Minuten (0 min).
    #[test]
    fn test_boundary_at_45s_switches_to_minutes() {
        let now = at(1_000_000);
        let then = now - Duration::from_secs(45);
        assert_eq!(relative_time(then, now), "vor 0 min");
    }

    /// 5 Minuten Differenz.
    #[test]
    fn test_five_minutes() {
        let now = at(1_000_000);
        let then = now - Duration::from_secs(5 * 60);
        assert_eq!(relative_time(then, now), "vor 5 min");
    }

    /// Kurz vor der 60-Minuten-Grenze bleibt es bei Minuten.
    #[test]
    fn test_boundary_just_below_one_hour() {
        let now = at(1_000_000);
        let then = now - Duration::from_secs(59 * 60 + 59);
        assert_eq!(relative_time(then, now), "vor 59 min");
    }

    /// Bei exakt 60 Minuten wechselt der Bucket zu Stunden (1 h).
    #[test]
    fn test_boundary_at_one_hour_switches_to_hours() {
        let now = at(1_000_000);
        let then = now - Duration::from_secs(60 * 60);
        assert_eq!(relative_time(then, now), "vor 1 h");
    }

    /// Kurz vor der 24-Stunden-Grenze bleibt es bei Stunden.
    #[test]
    fn test_boundary_just_below_one_day() {
        let now = at(1_000_000);
        let then = now - Duration::from_secs(23 * 60 * 60 + 59 * 60);
        assert_eq!(relative_time(then, now), "vor 23 h");
    }

    /// Bei exakt 24 Stunden wechselt der Bucket zu Tagen (1 d).
    #[test]
    fn test_boundary_at_one_day_switches_to_days() {
        let now = at(1_000_000);
        let then = now - Duration::from_secs(24 * 60 * 60);
        assert_eq!(relative_time(then, now), "vor 1 d");
    }

    /// Kurz vor der 7-Tage-Grenze bleibt es bei Tagen.
    #[test]
    fn test_boundary_just_below_seven_days() {
        let now = at(1_000_000_000);
        let then = now - Duration::from_secs(6 * 24 * 60 * 60 + 23 * 60 * 60);
        assert_eq!(relative_time(then, now), "vor 6 d");
    }

    /// Bei exakt 7 Tagen wechselt der Bucket zum Kalenderdatum.
    #[test]
    fn test_boundary_at_seven_days_switches_to_date() {
        // Bezugspunkt: 2000-01-08 UTC (10957 + 7 Tage seit Epoch), sieben Tage
        // nach dem bekannten Datum 2000-01-01 (Tag 10957 seit Epoch).
        let then = at(10_957 * 24 * 60 * 60);
        let now = at((10_957 + 7) * 24 * 60 * 60);
        assert_eq!(relative_time(then, now), "01.01.2000");
    }

    /// Zukünftige Zeitstempel (then nach now) gelten als "gerade eben".
    #[test]
    fn test_future_timestamp_is_just_now() {
        let now = at(1_000_000);
        let then = now + Duration::from_secs(3600);
        assert_eq!(relative_time(then, now), "gerade eben");
    }

    /// `civil_from_days` liefert für Tag 10957 das bekannte Datum 2000-01-01.
    #[test]
    fn test_civil_from_days_known_date() {
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }

    /// `civil_from_days` liefert für Tag 0 den Unix-Epoch 1970-01-01.
    #[test]
    fn test_civil_from_days_epoch() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    /// `format_utc_date` polstert Tag/Monat mit führenden Nullen.
    #[test]
    fn test_format_utc_date_padding() {
        // Tag 0 seit Epoch = 1970-01-01.
        assert_eq!(format_utc_date(at(0)), "01.01.1970");
    }
}
