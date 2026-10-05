//! Geheimnis-Maskierung für Umgebungen und Kommandozeilen.
//!
//! # Regeln
//! 1. **Name**: eine Variable/Option, deren Name (groß-/kleinschreibungsfrei)
//!    `KEY`, `TOKEN`, `SECRET`, `PASSWORD`, `PASSWD`, `PASSPHRASE`, `CREDENTIAL`,
//!    `PRIVATE`, `COOKIE`, `BEARER`, `AUTH`, `SIGNATURE`, `DSN`, `CERT` oder
//!    `LICENSE` enthält, wird maskiert (Ausnahmen: `AUTHOR`, `PWD`, `OLDPWD`,
//!    `KEYBOARD`, `MONKEY`, `*_KEYMAP`).
//! 2. **Wert**: Werte, die wie Geheimnisse aussehen (bekannte Präfixe wie
//!    `sk-`, `ghp_`, `AKIA…`, JWT `eyJ…`, PEM-Blöcke, URLs mit `user:pass@`,
//!    lange Zeichenfolgen ohne Trenner aus Buchstaben und Ziffern), werden
//!    maskiert — auch unter harmlosem Namen.
//! 3. **Kommandozeilen**: `--name=wert` und `--name wert` mit geheimem Namen,
//!    `NAME=wert`-Argumente und URLs mit Zugangsdaten werden maskiert.
//!
//! Maskiert wird mit [`MASK`]; die Länge des Werts bleibt unsichtbar. Es gibt
//! **keine** Möglichkeit, Werte offen auszugeben.
//!
//! # Grenzen
//! Eine Heuristik ist kein Beweis: ein Geheimnis unter harmlosem Namen und in
//! unbekanntem Format bleibt sichtbar. Deshalb gehören die `sys.*`-Werkzeuge
//! zur Stufe `ExecuteProcess` und nicht zu den automatisch freigegebenen.
//! Wer nach Teilen maskierter Werte filtert, filtert nur den maskierten Text,
//! damit Filter kein Orakel für das Original werden.

/// Ersatztext für maskierte Werte.
pub const MASK: &str = "***";

/// Namensbestandteile geheimer Variablen.
const SECRET_PARTS: &[&str] = &[
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "CREDENTIAL",
    "PRIVATE",
    "COOKIE",
    "BEARER",
    "AUTH",
    "SIGNATURE",
    "DSN",
    "CERT",
    "LICENSE",
];

/// Namen, die trotz Treffer nichts Geheimes sind.
const NOT_SECRET: &[&str] = &[
    "AUTHOR",
    "KEYBOARD",
    "MONKEY",
    "KEYMAP",
    "KEYRING_BACKEND",
    "SSH_AUTH_SOCK",
];

/// Bekannte Präfixe von Zugangsdaten.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xapp-",
    "AKIA",
    "ASIA",
    "AIza",
    "ya29.",
    "-----BEGIN",
    "npm_",
    "pypi-",
    "hf_",
    "dop_v1_",
    "SG.",
    "shpat_",
];

/// Ist `name` der Name eines geheimen Werts?
#[must_use]
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if upper == "PWD" || upper == "OLDPWD" {
        return false;
    }
    // `AUTHOR` (git) ist harmlos, `AUTHORIZATION` nicht.
    if upper.contains("AUTHORIZ") {
        return true;
    }
    let mut stripped = upper.clone();
    for allowed in NOT_SECRET {
        stripped = stripped.replace(allowed, "");
    }
    SECRET_PARTS.iter().any(|part| stripped.contains(part))
}

/// Sieht `value` wie ein Geheimnis aus (unabhängig vom Namen)?
#[must_use]
pub fn looks_like_secret_value(value: &str) -> bool {
    let v = value.trim();
    if v.len() < 8 {
        return false;
    }
    if SECRET_PREFIXES.iter().any(|prefix| v.starts_with(prefix)) {
        return true;
    }
    // JWT: drei Base64url-Teile, der erste beginnt mit `eyJ`.
    if v.starts_with("eyJ") && v.matches('.').count() == 2 {
        return true;
    }
    if url_has_credentials(v) {
        return true;
    }
    // Lange, trennerlose Folgen aus Buchstaben und Ziffern.
    if v.len() >= 32
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '='))
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    false
}

/// `scheme://user:pass@host` enthält Zugangsdaten.
#[must_use]
pub fn url_has_credentials(value: &str) -> bool {
    let Some((_, rest)) = value.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    match authority.rsplit_once('@') {
        Some((userinfo, _)) => userinfo.contains(':'),
        None => false,
    }
}

/// Ersetzt das Passwort einer URL mit Zugangsdaten durch [`MASK`].
#[must_use]
pub fn mask_url_password(value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return value.to_owned();
    };
    let split_at = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(split_at);
    match authority.rsplit_once('@') {
        Some((userinfo, host)) => match userinfo.split_once(':') {
            Some((user, _)) => format!("{scheme}://{user}:{MASK}@{host}{tail}"),
            None => value.to_owned(),
        },
        None => value.to_owned(),
    }
}

/// Maskiert den Wert einer Variablen: nach Name **oder** nach Aussehen.
#[must_use]
pub fn mask_env_value(name: &str, value: &str) -> (String, bool) {
    if is_secret_name(name) {
        return (MASK.to_owned(), true);
    }
    // URLs mit Zugangsdaten bleiben lesbar (Benutzer, Host, Pfad), nur das Passwort fällt weg.
    if url_has_credentials(value) {
        return (mask_url_password(value), true);
    }
    if looks_like_secret_value(value) {
        return (MASK.to_owned(), true);
    }
    (value.to_owned(), false)
}

/// Maskiert eine Kommandozeile argumentweise.
#[must_use]
pub fn mask_cmdline(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut mask_next = false;
    for arg in args {
        if mask_next {
            out.push(MASK.to_owned());
            mask_next = false;
            continue;
        }
        if let Some(flag) = arg.strip_prefix("--").or_else(|| arg.strip_prefix('-')) {
            if let Some((name, value)) = flag.split_once('=') {
                if is_secret_name(name) || looks_like_secret_value(value) {
                    let dashes = &arg[..arg.len() - flag.len()];
                    out.push(format!("{dashes}{name}={MASK}"));
                } else if url_has_credentials(value) {
                    let dashes = &arg[..arg.len() - flag.len()];
                    out.push(format!("{dashes}{name}={}", mask_url_password(value)));
                } else {
                    out.push(arg.clone());
                }
                continue;
            }
            if arg.starts_with("--") && is_secret_name(flag) {
                out.push(arg.clone());
                mask_next = true;
                continue;
            }
            out.push(arg.clone());
            continue;
        }
        if let Some((name, value)) = arg.split_once('=') {
            let name_ok = !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && name.chars().next().is_some_and(|c| !c.is_ascii_digit());
            if name_ok && (is_secret_name(name) || looks_like_secret_value(value)) {
                out.push(format!("{name}={MASK}"));
                continue;
            }
        }
        // HTTP-Kopfzeilen wie `Authorization: Bearer …`.
        if let Some((name, _)) = arg.split_once(':') {
            if !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
                && is_secret_name(name)
            {
                out.push(format!("{name}: {MASK}"));
                continue;
            }
        }
        if looks_like_secret_value(arg) {
            out.push(MASK.to_owned());
        } else if url_has_credentials(arg) {
            out.push(mask_url_password(arg));
        } else {
            out.push(arg.clone());
        }
    }
    out
}

/// Gekürzte, maskierte Kommandozeile als ein String (höchstens `max_chars` Zeichen).
#[must_use]
pub fn masked_command(args: &[String], max_chars: usize) -> String {
    let joined = mask_cmdline(args).join(" ");
    if joined.chars().count() <= max_chars {
        return joined;
    }
    let mut short: String = joined.chars().take(max_chars.saturating_sub(1)).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| (*i).to_owned()).collect()
    }

    #[test]
    fn secret_names_are_detected_and_known_false_positives_skipped() -> TestResult {
        for name in [
            "API_KEY",
            "github_token",
            "AWS_SECRET_ACCESS_KEY",
            "DB_PASSWORD",
            "PASSWD",
            "MY_PASSPHRASE",
            "GOOGLE_CREDENTIALS",
            "PRIVATE_KEY",
            "COOKIE",
            "BEARER",
            "HARW_AUTH",
            "SENTRY_DSN",
            "TLS_CERT",
            "x-api-key",
        ] {
            assert!(is_secret_name(name), "{name}");
        }
        for name in [
            "PATH",
            "HOME",
            "PWD",
            "OLDPWD",
            "GIT_AUTHOR_NAME",
            "SSH_AUTH_SOCK",
            "KEYBOARD",
            "USER",
            "LANG",
            "TERM",
        ] {
            assert!(!is_secret_name(name), "{name}");
        }
        Ok(())
    }

    #[test]
    fn secret_looking_values_are_detected_under_innocent_names() -> TestResult {
        for value in [
            "sk-abcdefghijklmnop",
            "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
            "AKIAIOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig",
            "-----BEGIN PRIVATE KEY-----",
            "postgres://user:hunter2@db.example.com/app",
            "0123456789abcdef0123456789abcdef0123",
        ] {
            assert!(looks_like_secret_value(value), "{value}");
            let (masked, was) = mask_env_value("HARMLESS", value);
            assert!(
                was && !masked.contains("hunter2") && !masked.contains("0123456789abcdef"),
                "{value} -> {masked}"
            );
        }
        for value in [
            "/usr/local/bin:/usr/bin",
            "en_US.UTF-8",
            "xterm-256color",
            "short",
            "https://example.com/path",
            "/home/user/some/very/long/path/that/is/not/a/secret/at/all",
        ] {
            assert!(!looks_like_secret_value(value), "{value}");
        }
        Ok(())
    }

    #[test]
    fn url_passwords_are_masked_but_user_and_host_stay() -> TestResult {
        assert_eq!(
            mask_url_password("postgres://app:hunter2@db:5432/x?sslmode=disable"),
            "postgres://app:***@db:5432/x?sslmode=disable"
        );
        assert_eq!(
            mask_url_password("https://example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            mask_url_password("https://user@example.com"),
            "https://user@example.com"
        );
        assert!(!url_has_credentials("https://example.com/a@b:c"));
        Ok(())
    }

    #[test]
    fn env_values_never_leak() -> TestResult {
        assert_eq!(mask_env_value("API_TOKEN", "abc"), (MASK.to_owned(), true));
        assert_eq!(
            mask_env_value("HOME", "/home/u"),
            ("/home/u".to_owned(), false)
        );
        let (masked, was) = mask_env_value("DATABASE_URL", "mysql://u:pw@h/d");
        assert!(was && !masked.contains("pw"), "{masked}");
        Ok(())
    }

    #[test]
    fn command_lines_are_masked() -> TestResult {
        let masked = mask_cmdline(&s(&[
            "curl",
            "--header",
            "x",
            "--password=hunter2",
            "--token",
            "abc123",
            "-H",
            "Authorization: Bearer abcdef",
            "API_KEY=zzz",
            "https://u:pw@h/x",
            "--url=https://u:pw2@h",
            "--verbose",
            "--api-key=k",
        ]));
        let joined = masked.join(" ");
        for leaked in ["hunter2", "abc123", "zzz", "pw@", "pw2", "=k ", "abcdef"] {
            assert!(!joined.contains(leaked), "{leaked} leaked in {joined}");
        }
        assert!(joined.contains("--password=***"));
        assert!(joined.contains("--token ***"));
        assert!(joined.contains("API_KEY=***"));
        assert!(joined.contains("Authorization: ***"), "{joined}");
        assert!(joined.contains("--verbose"));
        assert!(joined.contains("curl"));
        Ok(())
    }

    #[test]
    fn masked_command_is_clipped() -> TestResult {
        let long = s(&["echo", &"x".repeat(1000)]);
        let text = masked_command(&long, 50);
        assert_eq!(text.chars().count(), 50);
        assert!(text.ends_with('…'));
        Ok(())
    }
}
