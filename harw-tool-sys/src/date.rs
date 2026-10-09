//! `sys.date` — `date` in reinem Rust.
//!
//! Zeit in UTC oder mit festem Offset (`offset_minutes`, ±14 h); eine
//! Zeitzonen-Datenbank gibt es bewusst nicht (keine Abhängigkeit, keine
//! Hostdatei). `format` unterstützt eine strftime-Teilmenge; ein unbekannter
//! `%`-Spezifizierer ist ein Fehler (nicht still ignoriert).
//!
//! Unterstützt: `%%` `%a` `%A` `%b` `%h` `%B` `%d` `%e` `%D` `%F` `%H` `%I` `%j`
//! `%k` `%l` `%m` `%M` `%n` `%N` `%p` `%P` `%R` `%s` `%S` `%t` `%T` `%u` `%w`
//! `%y` `%Y` `%z` `%Z`.

use harw_tool_fsread::blocking::run_blocking;
use harw_tool_fsread::budget::{fail, ok};
use harw_tool_fsread::meta::{Civil, civil_utc};
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::Deserialize;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

/// Name des Werkzeugs.
pub const TOOL: &str = "sys.date";

/// Größter Offset in Minuten.
pub const MAX_OFFSET_MINUTES: i64 = 14 * 60;

/// Höchstlänge des Formats.
pub const MAX_FORMAT_CHARS: usize = 128;

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Argumente für `sys.date`.
#[derive(Debug, Deserialize, harw_macros::Tool)]
#[serde(deny_unknown_fields)]
pub struct DateArgs {
    /// strftime-style format, e.g. '%Y-%m-%d %H:%M:%S' (at most 128 characters; unknown specifiers are rejected).
    #[serde(default)]
    pub format: Option<String>,
    /// Fixed UTC offset in minutes (-840..840); default 0 (UTC).
    #[serde(default)]
    pub offset_minutes: Option<i64>,
    /// -d @N: format this Unix timestamp (seconds) instead of the current time.
    #[serde(default)]
    pub epoch_secs: Option<i64>,
}

fn offset_text(offset_minutes: i64, colon: bool) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.abs();
    if colon {
        format!("{sign}{:02}:{:02}", abs / 60, abs % 60)
    } else {
        format!("{sign}{:02}{:02}", abs / 60, abs % 60)
    }
}

/// Formatiert `civil` (bereits um den Offset verschoben) nach `format`.
///
/// # Errors
/// Meldung bei unbekanntem Spezifizierer oder abgebrochenem `%` am Ende.
pub fn strftime(
    format: &str,
    civil: &Civil,
    epoch: i64,
    nanos: u32,
    offset_minutes: i64,
) -> Result<String, String> {
    let weekday = WEEKDAYS[civil.weekday as usize % 7];
    let month = MONTHS[(civil.month as usize).saturating_sub(1) % 12];
    let hour12 = match civil.hour % 12 {
        0 => 12,
        other => other,
    };
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(spec) = chars.next() else {
            return Err("format ends with a lone '%'".to_owned());
        };
        match spec {
            '%' => out.push('%'),
            'a' => out.push_str(&weekday[..3]),
            'A' => out.push_str(weekday),
            'b' | 'h' => out.push_str(&month[..3]),
            'B' => out.push_str(month),
            'd' => out.push_str(&format!("{:02}", civil.day)),
            'e' => out.push_str(&format!("{:2}", civil.day)),
            'D' => out.push_str(&format!(
                "{:02}/{:02}/{:02}",
                civil.month,
                civil.day,
                civil.year.rem_euclid(100)
            )),
            'F' => out.push_str(&format!(
                "{:04}-{:02}-{:02}",
                civil.year, civil.month, civil.day
            )),
            'H' => out.push_str(&format!("{:02}", civil.hour)),
            'I' => out.push_str(&format!("{hour12:02}")),
            'j' => out.push_str(&format!("{:03}", civil.yday)),
            'k' => out.push_str(&format!("{:2}", civil.hour)),
            'l' => out.push_str(&format!("{hour12:2}")),
            'm' => out.push_str(&format!("{:02}", civil.month)),
            'M' => out.push_str(&format!("{:02}", civil.minute)),
            'n' => out.push('\n'),
            'N' => out.push_str(&format!("{nanos:09}")),
            'p' => out.push_str(if civil.hour < 12 { "AM" } else { "PM" }),
            'P' => out.push_str(if civil.hour < 12 { "am" } else { "pm" }),
            'R' => out.push_str(&format!("{:02}:{:02}", civil.hour, civil.minute)),
            's' => out.push_str(&epoch.to_string()),
            'S' => out.push_str(&format!("{:02}", civil.second)),
            't' => out.push('\t'),
            'T' => out.push_str(&format!(
                "{:02}:{:02}:{:02}",
                civil.hour, civil.minute, civil.second
            )),
            'u' => out.push_str(&(if civil.weekday == 0 { 7 } else { civil.weekday }).to_string()),
            'w' => out.push_str(&civil.weekday.to_string()),
            'y' => out.push_str(&format!("{:02}", civil.year.rem_euclid(100))),
            'Y' => out.push_str(&civil.year.to_string()),
            'z' => out.push_str(&offset_text(offset_minutes, false)),
            'Z' => out.push_str(&if offset_minutes == 0 {
                "UTC".to_owned()
            } else {
                offset_text(offset_minutes, true)
            }),
            other => return Err(format!("unsupported format specifier '%{other}'")),
        }
    }
    Ok(out)
}

/// Führt `sys.date` aus; `now` ist `(Sekunden, Nanosekunden)` seit der Epoche.
#[must_use]
pub fn run(now: (i64, u32), args: &DateArgs) -> ToolOutput {
    let offset = args.offset_minutes.unwrap_or(0);
    if !(-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&offset) {
        return fail(
            TOOL,
            format!(
                "offset_minutes must be between -{MAX_OFFSET_MINUTES} and {MAX_OFFSET_MINUTES}, got {offset}"
            ),
        );
    }
    if let Some(format) = &args.format {
        if format.chars().count() > MAX_FORMAT_CHARS || format.contains('\0') {
            return fail(
                TOOL,
                format!("format must be at most {MAX_FORMAT_CHARS} characters without NUL"),
            );
        }
    }
    let (epoch, nanos) = match args.epoch_secs {
        Some(secs) => {
            // Bereich begrenzen, damit die Kalenderrechnung nicht überläuft (Jahr ±292 Mrd. wäre sinnlos).
            if !(-62_135_596_800..=253_402_300_799).contains(&secs) {
                return fail(TOOL, "epoch_secs must be between year 1 and 9999");
            }
            (secs, 0)
        }
        None => now,
    };
    let shifted = epoch + offset * 60;
    let civil = civil_utc(shifted);
    let default_format = "%a %b %e %H:%M:%S %Z %Y";
    let format = args.format.as_deref().unwrap_or(default_format);
    let formatted = match strftime(format, &civil, epoch, nanos, offset) {
        Ok(text) => text,
        Err(message) => return fail(TOOL, message),
    };
    let iso = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}",
        civil.year,
        civil.month,
        civil.day,
        civil.hour,
        civil.minute,
        civil.second,
        if offset == 0 {
            "Z".to_owned()
        } else {
            offset_text(offset, true)
        }
    );
    ok(
        TOOL,
        formatted.clone(),
        json!({
            "formatted": formatted,
            "iso": iso,
            "epoch": epoch,
            "nanos": nanos,
            "weekday": WEEKDAYS[civil.weekday as usize % 7],
            "offset_minutes": offset,
            "truncated": false,
        }),
    )
}

/// Zeigt Datum und Uhrzeit wie `date`.
#[harw_macros::tool(
    name = "sys.date",
    description = "Reports the current date and time (or a given Unix timestamp) like date: UTC or a fixed offset_minutes, a strftime-style format (%Y-%m-%d %H:%M:%S %z %Z %s %j %a %b and more; unknown specifiers are rejected) and epoch seconds. Use when you need the current time, a Unix timestamp or a formatted date instead of running date. Returns JSON {formatted, iso, epoch, weekday}. No time zone database: only UTC and fixed offsets. Read-only.",
    permission = "execute_process",
    parallel_safe
)]
async fn sys_date(
    _context: &ToolExecutionContext,
    args: DateArgs,
) -> Result<ToolOutput, ToolsError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or((0, 0), |d| {
            (i64::try_from(d.as_secs()).unwrap_or(0), d.subsec_nanos())
        });
    run_blocking(TOOL, move || run(now, &args)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, error_of, json_of};
    use serde_json::Value;

    // 2026-10-05 11:34:56 UTC, Montag.
    const T: i64 = 1_791_200_096;

    fn date(args: Value) -> TestResult<Value> {
        json_of(run((T, 123_456_789), &serde_json::from_value(args)?))
    }

    #[test]
    fn formats_utc_default_and_iso() -> TestResult {
        let value = date(json!({}))?;
        assert_eq!(value["formatted"], "Mon Oct  5 11:34:56 UTC 2026");
        assert_eq!(value["iso"], "2026-10-05T11:34:56Z");
        assert_eq!(value["epoch"], T);
        assert_eq!(value["weekday"], "Monday");
        Ok(())
    }

    #[test]
    fn strftime_subset() -> TestResult {
        let f = |format: &str| -> TestResult<String> {
            Ok(date(json!({"format": format}))?["formatted"]
                .as_str()
                .unwrap_or("")
                .to_owned())
        };
        assert_eq!(f("%Y-%m-%d %H:%M:%S")?, "2026-10-05 11:34:56");
        assert_eq!(f("%F %T")?, "2026-10-05 11:34:56");
        assert_eq!(f("%D %R")?, "10/05/26 11:34");
        assert_eq!(f("%A %a %B %b %h")?, "Monday Mon October Oct Oct");
        assert_eq!(f("%j %u %w %y %e %k %l")?, "278 1 1 26  5 11 11");
        assert_eq!(f("%I %p %P")?, "11 AM am");
        assert_eq!(f("%s %N")?, format!("{T} 123456789"));
        assert_eq!(f("100%% %z %Z")?, "100% +0000 UTC");
        assert_eq!(f("a%tb%nc")?, "a\tb\nc");
        Ok(())
    }

    #[test]
    fn offsets_shift_the_calendar() -> TestResult {
        let value = date(json!({"offset_minutes": 90, "format": "%H:%M %z %Z %F"}))?;
        assert_eq!(value["formatted"], "13:04 +0130 +01:30 2026-10-05");
        assert_eq!(value["iso"], "2026-10-05T13:04:56+01:30");
        let value = date(json!({"offset_minutes": -720, "format": "%F %H"}))?;
        assert_eq!(value["formatted"], "2026-10-04 23");
        let value = date(json!({"epoch_secs": 0, "format": "%F %T %a"}))?;
        assert_eq!(value["formatted"], "1970-01-01 00:00:00 Thu");
        let value = date(json!({"epoch_secs": -1, "format": "%F %T"}))?;
        assert_eq!(value["formatted"], "1969-12-31 23:59:59");
        Ok(())
    }

    #[test]
    fn noon_midnight_and_leap_day() -> TestResult {
        let f = |secs: i64, format: &str| -> TestResult<String> {
            Ok(
                date(json!({"epoch_secs": secs, "format": format}))?["formatted"]
                    .as_str()
                    .unwrap_or("")
                    .to_owned(),
            )
        };
        assert_eq!(f(951_782_400, "%F %j")?, "2000-02-29 060");
        assert_eq!(f(951_782_400 + 12 * 3600, "%I %p")?, "12 PM");
        assert_eq!(f(951_782_400, "%I %p")?, "12 AM");
        Ok(())
    }

    #[test]
    fn invalid_input_is_rejected() -> TestResult {
        for bad in [
            json!({"format": "%Q"}),
            json!({"format": "abc%"}),
            json!({"format": "%Ez"}),
            json!({"format": "x".repeat(MAX_FORMAT_CHARS + 1)}),
            json!({"format": "a\u{0}"}),
            json!({"offset_minutes": 841}),
            json!({"offset_minutes": i64::MIN}),
            json!({"epoch_secs": i64::MAX}),
            json!({"epoch_secs": i64::MIN}),
        ] {
            error_of(run((T, 0), &serde_json::from_value(bad.clone())?))
                .map_err(|e| crate::test_support::TestError::Unexpected(format!("{bad}: {e}")))?;
        }
        assert!(serde_json::from_value::<DateArgs>(json!({"tz": "Europe/Berlin"})).is_err());
        Ok(())
    }
}
