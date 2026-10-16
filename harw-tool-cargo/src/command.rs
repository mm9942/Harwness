//! Aufbau und Absicherung der Kommandozeile.
//!
//! # Verantwortung
//! Jedes Argument, das in das `cargo`-Kommando fließt, wird hier **validiert**
//! (feste Zeichenmengen, keine führenden `-`, Längenbegrenzung) und danach
//! POSIX-sicher in Einfachanführungszeichen gesetzt. Es gibt keinen Weg, rohen
//! Text des Aufrufers in die Shell zu bringen:
//!
//! - Namen (`package`, `bin`, `exclude`, `invert`) und Features sind auf
//!   Bezeichnerzeichen begrenzt,
//! - der Testfilter darf kein `-` am Anfang haben (keine Optionsinjektion),
//! - Argumente hinter `--` stammen **ausschließlich** aus einer Allowlist
//!   ([`harness_arg`], [`lint_flag`]),
//! - Steuerzeichen, Leerraum und NUL sind überall verboten.
//!
//! Das Ergebnis ist ein fester `argv`; [`join`] macht daraus den
//! Kommando-String für `shell.exec`.

/// Höchstzahl Einträge je Liste.
pub const MAX_LIST: usize = 64;

/// Setzt `value` in POSIX-Einfachanführungszeichen.
#[must_use]
pub fn sh_quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Verbindet `argv` zu einem Shell-Kommando; jedes Argument wird gequotet,
/// sofern es nicht nur aus sicheren Zeichen besteht.
#[must_use]
pub fn join(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| {
            let safe = !arg.is_empty()
                && arg.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | ',' | '+' | '@')
                });
            if safe { arg.clone() } else { sh_quote(arg) }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn clip(value: &str) -> String {
    value.chars().take(48).collect()
}

/// Gemeinsame Prüfung: nicht leer, nicht zu lang, kein Leerraum/Steuerzeichen.
fn plain(kind: &str, value: &str, max: usize) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{kind} must not be empty"));
    }
    if value.len() > max {
        return Err(format!("{kind} longer than {max} bytes"));
    }
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{kind} '{}' must not contain whitespace or control characters",
            clip(value)
        ));
    }
    if value.starts_with('-') {
        return Err(format!("{kind} '{}' must not start with '-'", clip(value)));
    }
    Ok(())
}

/// Paketname oder Paketspezifikation (`name` oder `name@version`).
///
/// # Errors
/// Meldung bei ungültigen Zeichen.
pub fn package(value: &str) -> Result<(), String> {
    plain("package", value, 128)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '@'))
    {
        return Err(format!(
            "package '{}' may only contain letters, digits and _ - . + @",
            clip(value)
        ));
    }
    if value.matches('@').count() > 1 {
        return Err(format!("package '{}' has more than one '@'", clip(value)));
    }
    Ok(())
}

/// Name eines Binärziels (`--bin`).
///
/// # Errors
/// Meldung bei ungültigen Zeichen.
pub fn target_name(value: &str) -> Result<(), String> {
    plain("target name", value, 128)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(format!(
            "target name '{}' may only contain letters, digits, _ and -",
            clip(value)
        ));
    }
    Ok(())
}

/// Feature-Name (`a`, `pkg/feat`, `pkg?/feat`, `dep:name`).
///
/// # Errors
/// Meldung bei ungültigen Zeichen.
pub fn feature(value: &str) -> Result<(), String> {
    plain("feature", value, 128)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '?' | '+'))
    {
        return Err(format!(
            "feature '{}' may only contain letters, digits and _ - . / : ? +",
            clip(value)
        ));
    }
    Ok(())
}

/// Profilname (`--profile`).
///
/// # Errors
/// Meldung bei ungültigen Zeichen.
pub fn profile(value: &str) -> Result<(), String> {
    plain("profile", value, 64)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(format!(
            "profile '{}' may only contain letters, digits, _ and -",
            clip(value)
        ));
    }
    Ok(())
}

/// Testfilter (positionales `TESTNAME`).
///
/// # Errors
/// Meldung bei ungültigen Zeichen oder führendem `-`.
pub fn test_filter(value: &str) -> Result<(), String> {
    plain("test_filter", value, 200)?;
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.' | '/' | '-'))
    {
        return Err(format!(
            "test_filter '{}' may only contain letters, digits and _ : . / -",
            clip(value)
        ));
    }
    Ok(())
}

/// Lint-Name (`warnings`, `unused_variables`, `clippy::pedantic`).
///
/// # Errors
/// Meldung bei ungültigem Namen.
pub fn lint_name(value: &str) -> Result<(), String> {
    plain("lint", value, 96)?;
    let ok = value.split("::").all(|part| {
        !part.is_empty()
            && part
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }) && value.split("::").count() <= 2;
    if !ok {
        return Err(format!(
            "lint '{}' must look like name or tool::name (lowercase letters, digits, _)",
            clip(value)
        ));
    }
    Ok(())
}

/// Erlaubte Argumente für den Testlauf hinter `--` (Allowlist).
///
/// Erlaubt: `--nocapture`, `--show-output`, `--ignored`, `--include-ignored`,
/// `--exact`, `--quiet`/`-q`, `--test-threads=N` (1..=256), `--skip=NAME`.
///
/// # Errors
/// Meldung, welche Argumente erlaubt sind.
pub fn harness_arg(value: &str) -> Result<(), String> {
    const FLAGS: &[&str] = &[
        "--nocapture",
        "--show-output",
        "--ignored",
        "--include-ignored",
        "--exact",
        "--quiet",
        "-q",
    ];
    if FLAGS.contains(&value) {
        return Ok(());
    }
    if let Some(threads) = value.strip_prefix("--test-threads=") {
        return match threads.parse::<u32>() {
            Ok(n) if (1..=256).contains(&n) => Ok(()),
            _ => Err("--test-threads needs a number between 1 and 256".to_owned()),
        };
    }
    if let Some(name) = value.strip_prefix("--skip=") {
        return test_filter(name);
    }
    Err(format!(
        "harness argument '{}' is not allowed; allowed: {}, --test-threads=N, --skip=NAME",
        clip(value),
        FLAGS.join(" ")
    ))
}

/// Ein Lint-Flag aus Stufe und Name (`-D`, `-W`, `-A`).
///
/// # Errors
/// Meldung bei ungültigem Namen.
pub fn lint_flag(level: char, name: &str) -> Result<[String; 2], String> {
    lint_name(name)?;
    Ok([format!("-{level}"), name.to_owned()])
}

/// Mögliche Kanten von `cargo tree -e`.
pub const TREE_EDGES: &[&str] = &[
    "normal",
    "build",
    "dev",
    "features",
    "all",
    "no-normal",
    "no-build",
    "no-dev",
    "no-proc-macro",
    "proc-macro",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn quoting_is_posix_safe() -> TestResult {
        assert_eq!(sh_quote("abc"), "'abc'");
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
        assert_eq!(sh_quote("$(rm -rf /)"), "'$(rm -rf /)'");
        assert_eq!(sh_quote(""), "''");
        let joined = join(&[
            "cargo".into(),
            "check".into(),
            "-p".into(),
            "a b".into(),
            "--features=x,y".into(),
            "".into(),
            "it's".into(),
        ]);
        assert_eq!(joined, "cargo check -p 'a b' --features=x,y '' 'it'\\''s'");
        Ok(())
    }

    #[test]
    fn names_reject_injection() -> TestResult {
        for good in ["harw-core", "serde_json", "serde@1.0.228", "a.b+c"] {
            package(good).map_err(crate::test_support::TestError::Unexpected)?;
        }
        for bad in [
            "",
            "a b",
            "a;b",
            "$(x)",
            "`x`",
            "a&b",
            "a|b",
            "a>b",
            "a\nb",
            "a\tb",
            "-p",
            "--config=x",
            "a@b@c",
            "a\u{0}b",
            "é",
            &"a".repeat(129),
            "a'b",
            "a\"b",
            "a\\b",
            "~",
            "a*b",
        ] {
            assert!(package(bad).is_err(), "{bad:?} must be rejected");
        }
        for bad in ["a b", "a/b", "-x", "a;b", ""] {
            assert!(target_name(bad).is_err(), "{bad:?}");
        }
        assert!(target_name("my-bin_2").is_ok());
        for good in ["fancy", "serde/derive", "dep:foo", "pkg?/feat", "a-b_c.d"] {
            feature(good).map_err(crate::test_support::TestError::Unexpected)?;
        }
        for bad in ["a b", "a,b", "-x", "a;b", "", "a=b"] {
            assert!(feature(bad).is_err(), "{bad:?}");
        }
        for bad in ["a b", "-x", "a/b", "a.b", ""] {
            assert!(profile(bad).is_err(), "{bad:?}");
        }
        assert!(profile("release-lto").is_ok());
        Ok(())
    }

    #[test]
    fn test_filter_cannot_become_an_option() -> TestResult {
        for good in ["tests::passes", "a/b", "my-test", "x.y"] {
            test_filter(good).map_err(crate::test_support::TestError::Unexpected)?;
        }
        for bad in ["--nocapture", "-x", "a b", "a;b", "$(x)", "a*", "", "a\nb"] {
            assert!(test_filter(bad).is_err(), "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn harness_args_are_an_allowlist() -> TestResult {
        for good in [
            "--nocapture",
            "--show-output",
            "--ignored",
            "--include-ignored",
            "--exact",
            "--quiet",
            "-q",
            "--test-threads=1",
            "--test-threads=256",
            "--skip=slow_test",
        ] {
            harness_arg(good).map_err(crate::test_support::TestError::Unexpected)?;
        }
        for bad in [
            "--test-threads=0",
            "--test-threads=257",
            "--test-threads=x",
            "--skip=--x",
            "--skip=a b",
            "--format=json",
            "-Zunstable-options",
            "--list",
            "--bench",
            "--exec=sh",
            "; rm -rf /",
            "--nocapture --exact",
            "--logfile=/etc/passwd",
            "",
        ] {
            assert!(harness_arg(bad).is_err(), "{bad:?} must be rejected");
        }
        Ok(())
    }

    #[test]
    fn lint_names_are_restricted() -> TestResult {
        for good in [
            "warnings",
            "unused_variables",
            "clippy::pedantic",
            "clippy::needless_return",
            "rust_2018_idioms",
        ] {
            lint_name(good).map_err(crate::test_support::TestError::Unexpected)?;
        }
        for bad in [
            "",
            "A",
            "a b",
            "clippy::a::b",
            "-D",
            "a;b",
            "clippy::",
            "::a",
            "a-b",
            "$(x)",
        ] {
            assert!(lint_name(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            lint_flag('D', "warnings").map_err(crate::test_support::TestError::Unexpected)?,
            ["-D".to_owned(), "warnings".to_owned()]
        );
        Ok(())
    }
}
