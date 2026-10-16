//! Gemeinsame Formatierung: Zeiten, Commit-JSON, Fristen.

use crate::object::{Commit, Signature};
use crate::oid::Oid;
use harw_tool_fsread::budget::clip_text;
use harw_tool_fsread::meta::{civil_utc, days_from_civil};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Frist für einen Werkzeugaufruf.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(20);

/// Größe des Nachrichtentextes je Commit in Listen.
pub const BODY_CLIP: usize = 1024;

/// Fristende ab jetzt.
#[must_use]
pub fn deadline() -> Instant {
    Instant::now() + CALL_TIMEOUT
}

/// `2026-10-05T12:34:56+02:00` aus Unix-Sekunden und Zeitzonenversatz in Minuten.
#[must_use]
pub fn iso_with_tz(when: i64, tz_minutes: i32) -> String {
    let local = when.clamp(-(1 << 40), 1 << 40) + i64::from(tz_minutes) * 60;
    let c = civil_utc(local);
    let sign = if tz_minutes < 0 { '-' } else { '+' };
    let abs = tz_minutes.unsigned_abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{:02}:{:02}",
        c.year,
        c.month,
        c.day,
        c.hour,
        c.minute,
        c.second,
        abs / 60,
        abs % 60
    )
}

/// Liest `1700000000`, `2023-11-14`, `2023-11-14T22:13:20`, `2023-11-14 22:13:20`
/// (optional mit `Z`) als Unix-Sekunden (UTC).
#[must_use]
pub fn parse_when(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() || text.len() > 40 {
        return None;
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        return text.parse().ok();
    }
    let text = text.trim_end_matches('Z');
    let (date, time) = match text.split_once(['T', ' ']) {
        Some((d, t)) => (d, Some(t)),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some()
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(1..=9999).contains(&year)
    {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * 86_400;
    if let Some(time) = time {
        let mut fields = time.split(':');
        let h: i64 = fields.next()?.parse().ok()?;
        let m: i64 = fields.next().unwrap_or("0").parse().ok()?;
        let s: i64 = fields.next().unwrap_or("0").parse().ok()?;
        if fields.next().is_some()
            || !(0..24).contains(&h)
            || !(0..60).contains(&m)
            || !(0..61).contains(&s)
        {
            return None;
        }
        secs += h * 3600 + m * 60 + s;
    }
    Some(secs)
}

/// Signatur als JSON.
#[must_use]
pub fn signature_json(sig: &Signature) -> Value {
    json!({"name": sig.name, "email": sig.email, "date": iso_with_tz(sig.when, sig.tz_minutes), "epoch": sig.when})
}

/// Commit-Kopf als JSON; `body_clip` begrenzt den Nachrichtentext.
#[must_use]
pub fn commit_json(oid: &Oid, commit: &Commit, body_clip: usize) -> Value {
    let mut value = json!({
        "commit": oid.hex(),
        "short": oid.short(),
        "parents": commit.parents.iter().map(Oid::hex).collect::<Vec<_>>(),
        "merge": commit.parents.len() > 1,
        "author": signature_json(&commit.author),
        "committer": signature_json(&commit.committer),
        "subject": clip_text(commit.subject(), 512).0,
    });
    let body = commit.body().trim_end();
    if !body.is_empty() && body_clip > 0 {
        let (text, clipped) = clip_text(body, body_clip);
        value["body"] = json!(text);
        if clipped {
            value["body_truncated"] = json!(true);
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn iso_dates_with_zones() -> TestResult {
        assert_eq!(iso_with_tz(1_700_000_000, 0), "2023-11-14T22:13:20+00:00");
        assert_eq!(iso_with_tz(1_700_000_000, 120), "2023-11-15T00:13:20+02:00");
        assert_eq!(
            iso_with_tz(1_700_000_000, -330),
            "2023-11-14T16:43:20-05:30"
        );
        assert_eq!(iso_with_tz(0, 0), "1970-01-01T00:00:00+00:00");
        assert!(iso_with_tz(i64::MAX, 600).len() > 10);
        Ok(())
    }

    #[test]
    fn parses_dates_and_rejects_nonsense() -> TestResult {
        assert_eq!(parse_when("1700000000"), Some(1_700_000_000));
        assert_eq!(parse_when("2023-11-14"), Some(1_699_920_000));
        assert_eq!(parse_when("2023-11-14T22:13:20Z"), Some(1_700_000_000));
        assert_eq!(
            parse_when("2023-11-14 22:13"),
            Some(1_699_920_000 + 22 * 3600 + 13 * 60)
        );
        for bad in [
            "",
            "yesterday",
            "2023-13-01",
            "2023-00-10",
            "2023-11-32",
            "2023-11-14T25:00:00",
            "2023-11-14-1",
            "99999999999999999999999",
            "2023-11-14T1:2:3:4",
        ] {
            assert_eq!(parse_when(bad), None, "{bad}");
        }
        Ok(())
    }
}
