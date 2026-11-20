//! Die Warden-eigene Konfiguration `/etc/harw-dod/warden.toml`:
//! [`WardenConfig`], [`parse_warden_config`], [`load_warden_config`]
//! (PL-100 WP 3.1, Leitprinzip I6/I7).
//!
//! # Zweck
//! Der Warden liest seine Policy selbst, aus einer root-eigenen Datei, die
//! weder gruppen- noch weltschreibbar ist (`trust.rs`). Kein anderer Prozess
//! (Escalator, harw, Sentinel) liefert ihm Regeln.
//!
//! # Schema (Version 1)
//! ```toml
//! schema = 1
//! # Peer-Allowlist: genau einer der beiden Schlüssel, Vorgabe "harw-escalator".
//! escalator_user = "harw-escalator"
//! # escalator_uid = 987
//! proof_key_dir = "/etc/harw-dod/warden-keys"
//! allowed_cgroup_prefixes = ["harw.slice"]
//! allowed_actions = ["freeze-cgroup", "thaw-cgroup"]
//! deny = ["harw.slice/protected-*.scope/**"]
//!
//! [proof]
//! max_ttl_secs = 60
//! max_clock_skew_secs = 5
//! ```
//! Pflicht: `schema = 1` und eine nicht leere `allowed_cgroup_prefixes`.
//! Alles andere hat Vorgaben (siehe [`WardenConfig`]). Unbekannte Schlüssel
//! und Tabellen sind ein Fehler.
//!
//! # Warum ein eigener, strenger TOML-Teilmengen-Parser
//! Die `toml`-Crate brächte rund acht zusätzliche Crates in die Hülle des
//! Wardens (`indexmap`, `hashbrown`, `winnow`, `toml_parser`, ...). Das
//! xtask-Gate `MAX_WARDEN_RUNTIME_DEPS` ist eine Sperrklinke, die nicht
//! steigen darf. Diese Datei versteht deshalb genau die Teilmenge, die das
//! Schema braucht: Kommentare (`#`), Tabellenköpfe `[name]`, `key = value`
//! mit nackten Schlüsseln, Strings in doppelten Anführungszeichen (nur die
//! Escapes `\"` und `\\`, keine Steuerzeichen), nicht negative Ganzzahlen
//! ohne Vorzeichen, Unterstriche und führende Nullen sowie String-Arrays
//! (auch mehrzeilig, mit optionalem abschließendem Komma). Wahrheitswerte,
//! Inline-Tabellen, Tabellen-Arrays und punktierte Schlüssel braucht das
//! Schema nicht; sie werden abgelehnt. Jede Datei, die dieser Parser annimmt, ist gültiges
//! TOML mit derselben Bedeutung; alles andere wird abgelehnt (fail closed).
//!
//! # Nebenläufigkeit
//! Reine Funktionen.
//!
//! # Fehler
//! [`WardenError::ConfigSyntax`] (mit Zeilennummer, ohne Dateiinhalt),
//! [`WardenError::ConfigInvalid`], [`WardenError::Proof`] (ungültige
//! [`ProofPolicy`]) und die Fehler aus `trust.rs`.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden::config::{EscalatorIdentity, parse_warden_config};
//!
//! let text = "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\n";
//! let Ok(config) = parse_warden_config(text) else { return };
//! assert_eq!(config.escalator, EscalatorIdentity::User("harw-escalator".to_owned()));
//! assert_eq!(config.policy.actions().kinds(), ["freeze-cgroup", "thaw-cgroup"]);
//! ```

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use harw_dod_warden_proto::ProofPolicy;

use crate::error::{WardenError, WardenResult};
use crate::policy::{ActionAllowlist, DenyList, WardenPolicy};
use crate::trust::{FileSecrecy, TrustRules, read_trusted_file};

/// Vorgabepfad der Warden-Policy.
pub const DEFAULT_POLICY_PATH: &str = "/etc/harw-dod/warden.toml";

/// Vorgabe für `proof_key_dir`.
pub const DEFAULT_PROOF_KEY_DIR: &str = "/etc/harw-dod/warden-keys";

/// Vorgabe für `escalator_user`.
pub const DEFAULT_ESCALATOR_USER: &str = "harw-escalator";

/// Vorgabe für `[proof] max_ttl_secs`.
pub const DEFAULT_MAX_TTL_SECS: u32 = 60;

/// Vorgabe für `[proof] max_clock_skew_secs`.
pub const DEFAULT_MAX_CLOCK_SKEW_SECS: u32 = 5;

/// Einzige unterstützte Schema-Version von `warden.toml`.
pub const CONFIG_SCHEMA_VERSION: u64 = 1;

// Größenobergrenze der Policy-Datei.
const MAX_CONFIG_LEN: u64 = 64 * 1024;

// Die einzigen Tabellen des Schemas (die Wurzeltabelle hat keinen Kopf).
const KNOWN_TABLES: [&str; 1] = ["proof"];

// Längste zulässige Benutzerkennung (wie `useradd`).
const MAX_USER_NAME_LEN: usize = 32;

/// Wer als Escalator an den Socket darf (Peer-Allowlist, `SO_PEERCRED`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalatorIdentity {
    /// Systembenutzer, dessen uid der Warden einmal beim Start auflöst.
    User(String),
    /// Feste uid (nie `0`).
    Uid(u32),
}

/// Die geprüfte Warden-Konfiguration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WardenConfig {
    /// Peer-Allowlist (Vorgabe: Benutzer `harw-escalator`).
    pub escalator: EscalatorIdentity,
    /// Ordner mit den Proof-Schlüsseln (absolut, ohne `.`/`..`).
    pub proof_key_dir: PathBuf,
    /// Die Durchsetzungs-Policy.
    pub policy: WardenPolicy,
}

/// Loads and parses the warden policy file after the trust checks.
///
/// # Arguments
/// - `path` (`&Path`): Pfad der Policy (Produktion: [`DEFAULT_POLICY_PATH`]).
/// - `rules` (`TrustRules`): erwarteter Eigentümer (Produktion: root).
///
/// # Errors
/// Alles aus [`read_trusted_file`] und [`parse_warden_config`]; nicht
/// UTF-8 ist [`WardenError::ConfigSyntax`] in Zeile 0.
pub fn load_warden_config(path: &Path, rules: TrustRules) -> WardenResult<WardenConfig> {
    let bytes = read_trusted_file(
        path,
        rules,
        FileSecrecy::Public,
        MAX_CONFIG_LEN,
        "warden policy",
    )?;
    let text = String::from_utf8(bytes).map_err(|_| WardenError::ConfigSyntax {
        line: 0,
        reason: "file is not valid UTF-8",
    })?;
    parse_warden_config(&text)
}

/// Parses the text of `warden.toml` (schema 1).
///
/// # Errors
/// - [`WardenError::ConfigSyntax`]: Verstoß gegen die TOML-Teilmenge,
///   doppelter oder unbekannter Schlüssel, unbekannte (auch leere) oder
///   doppelte Tabelle, falscher Werttyp.
/// - [`WardenError::ConfigInvalid`]: falsche Schema-Version, fehlende
///   Präfixe, `escalator_user` und `escalator_uid` gleichzeitig, uid `0`,
///   ungültiger Benutzername oder Schlüsselordner, ungültige Allowlist oder
///   ungültiges Deny-Muster.
/// - [`WardenError::Proof`]: TTL/Versatz/Präfixe außerhalb der Grenzen von
///   [`ProofPolicy::new`].
pub fn parse_warden_config(text: &str) -> WardenResult<WardenConfig> {
    let tokens = lex(text)?;
    let mut document = parse_document(&tokens)?;

    let schema = take_int(&mut document, "", "schema")?.ok_or(WardenError::ConfigInvalid {
        reason: "schema is missing",
    })?;
    if schema != CONFIG_SCHEMA_VERSION {
        return Err(WardenError::ConfigInvalid {
            reason: "unsupported schema version",
        });
    }

    let user = take_str(&mut document, "", "escalator_user")?;
    let uid = take_int(&mut document, "", "escalator_uid")?;
    let escalator = match (user, uid) {
        (Some(_), Some(_)) => {
            return Err(WardenError::ConfigInvalid {
                reason: "escalator_user and escalator_uid are mutually exclusive",
            });
        }
        (Some(name), None) => EscalatorIdentity::User(validate_user_name(name)?),
        (None, Some(uid)) => EscalatorIdentity::Uid(validate_uid(uid)?),
        (None, None) => EscalatorIdentity::User(DEFAULT_ESCALATOR_USER.to_owned()),
    };

    let proof_key_dir = match take_str(&mut document, "", "proof_key_dir")? {
        Some(dir) => validate_key_dir(&dir)?,
        None => PathBuf::from(DEFAULT_PROOF_KEY_DIR),
    };

    let prefixes =
        take_str_array(&mut document, "", "allowed_cgroup_prefixes")?.unwrap_or_default();
    if prefixes.is_empty() {
        return Err(WardenError::ConfigInvalid {
            reason: "allowed_cgroup_prefixes must name at least one prefix",
        });
    }
    let actions = match take_str_array(&mut document, "", "allowed_actions")? {
        Some(names) => ActionAllowlist::new(names)?,
        None => ActionAllowlist::freeze_and_thaw(),
    };
    let deny =
        DenyList::with_extra(&take_str_array(&mut document, "", "deny")?.unwrap_or_default())?;

    let max_ttl = take_int(&mut document, "proof", "max_ttl_secs")?
        .map(to_u32)
        .transpose()?
        .unwrap_or(DEFAULT_MAX_TTL_SECS);
    let max_skew = take_int(&mut document, "proof", "max_clock_skew_secs")?
        .map(to_u32)
        .transpose()?
        .unwrap_or(DEFAULT_MAX_CLOCK_SKEW_SECS);
    let proof = ProofPolicy::new(max_ttl, max_skew, prefixes)?;

    if let Some((_, entry)) = document.into_iter().next() {
        return Err(WardenError::ConfigSyntax {
            line: entry.line,
            reason: "unknown key",
        });
    }

    Ok(WardenConfig {
        escalator,
        proof_key_dir,
        policy: WardenPolicy::new(proof, actions, deny),
    })
}

// -- Werte und Prüfungen ------------------------------------------------------

// Ein geparster Wert der Teilmenge.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Str(String),
    Int(u64),
    StrArray(Vec<String>),
}

// Ein Eintrag mit seiner Zeilennummer (für Fehlermeldungen).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    value: Value,
    line: usize,
}

// (Tabelle, Schlüssel) → Eintrag; die Wurzeltabelle heißt "".
type Document = BTreeMap<(String, String), Entry>;

// Entnimmt einen String-Wert.
fn take_str(document: &mut Document, table: &str, key: &str) -> WardenResult<Option<String>> {
    match document.remove(&(table.to_owned(), key.to_owned())) {
        None => Ok(None),
        Some(Entry {
            value: Value::Str(value),
            ..
        }) => Ok(Some(value)),
        Some(entry) => Err(type_error(entry.line)),
    }
}

// Entnimmt einen Ganzzahl-Wert.
fn take_int(document: &mut Document, table: &str, key: &str) -> WardenResult<Option<u64>> {
    match document.remove(&(table.to_owned(), key.to_owned())) {
        None => Ok(None),
        Some(Entry {
            value: Value::Int(value),
            ..
        }) => Ok(Some(value)),
        Some(entry) => Err(type_error(entry.line)),
    }
}

// Entnimmt ein String-Array.
fn take_str_array(
    document: &mut Document,
    table: &str,
    key: &str,
) -> WardenResult<Option<Vec<String>>> {
    match document.remove(&(table.to_owned(), key.to_owned())) {
        None => Ok(None),
        Some(Entry {
            value: Value::StrArray(values),
            ..
        }) => Ok(Some(values)),
        Some(entry) => Err(type_error(entry.line)),
    }
}

// Fehler für einen Wert vom falschen Typ.
fn type_error(line: usize) -> WardenError {
    WardenError::ConfigSyntax {
        line,
        reason: "value has the wrong type",
    }
}

// Ganzzahl auf u32 begrenzen.
fn to_u32(value: u64) -> WardenResult<u32> {
    u32::try_from(value).map_err(|_| WardenError::ConfigInvalid {
        reason: "number is out of range",
    })
}

// Benutzername: 1–32 Zeichen aus [a-z0-9_.-], nicht mit `-` beginnend.
fn validate_user_name(name: String) -> WardenResult<String> {
    let well_formed = !name.is_empty()
        && name.len() <= MAX_USER_NAME_LEN
        && !name.starts_with('-')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
        });
    if well_formed {
        Ok(name)
    } else {
        Err(WardenError::ConfigInvalid {
            reason: "escalator_user is not a valid system user name",
        })
    }
}

// uid: nie root.
fn validate_uid(uid: u64) -> WardenResult<u32> {
    let uid = to_u32(uid)?;
    if uid == 0 {
        return Err(WardenError::ConfigInvalid {
            reason: "escalator_uid must not be root",
        });
    }
    Ok(uid)
}

// Schlüsselordner: absolut, nur normale Komponenten.
fn validate_key_dir(dir: &str) -> WardenResult<PathBuf> {
    let path = PathBuf::from(dir);
    let well_formed = path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
        && !dir.chars().any(char::is_control);
    if well_formed {
        Ok(path)
    } else {
        Err(WardenError::ConfigInvalid {
            reason: "proof_key_dir must be an absolute path without . or ..",
        })
    }
}

// -- Lexer ----------------------------------------------------------------------

// Ein Token der Teilmenge.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    LBracket,
    RBracket,
    Equals,
    Comma,
    Newline,
    Str(String),
    Word(String),
}

// Ein Token mit Zeilennummer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lexed {
    token: Token,
    line: usize,
}

// Fehler im Lexer/Parser.
fn syntax(line: usize, reason: &'static str) -> WardenError {
    WardenError::ConfigSyntax { line, reason }
}

// Zeichen eines nackten Schlüssels bzw. Wortes (Zahl).
fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

// Zerlegt den Text in Tokens.
fn lex(text: &str) -> WardenResult<Vec<Lexed>> {
    let mut tokens = Vec::new();
    let mut line: usize = 1;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let token = match c {
            '\n' => Token::Newline,
            ' ' | '\t' => continue,
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    continue;
                }
                return Err(syntax(line, "carriage return outside a line break"));
            }
            '#' => {
                while let Some(&next) = chars.peek() {
                    if next == '\n' {
                        break;
                    }
                    if next.is_control() && next != '\t' && next != '\r' {
                        return Err(syntax(line, "control character in a comment"));
                    }
                    chars.next();
                }
                continue;
            }
            '[' => Token::LBracket,
            ']' => Token::RBracket,
            '=' => Token::Equals,
            ',' => Token::Comma,
            '"' => Token::Str(lex_string(&mut chars, line)?),
            c if is_word_char(c) => {
                let mut word = String::from(c);
                while let Some(&next) = chars.peek() {
                    if !is_word_char(next) {
                        break;
                    }
                    word.push(next);
                    chars.next();
                }
                Token::Word(word)
            }
            _ => return Err(syntax(line, "unexpected character")),
        };
        let is_newline = token == Token::Newline;
        tokens.push(Lexed { token, line });
        if is_newline {
            line = line.saturating_add(1);
        }
    }
    Ok(tokens)
}

// Liest einen String nach dem öffnenden `"` bis zum schließenden.
fn lex_string(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    line: usize,
) -> WardenResult<String> {
    let mut value = String::new();
    loop {
        match chars.next() {
            None | Some('\n') => return Err(syntax(line, "unterminated string")),
            Some('"') => return Ok(value),
            Some('\\') => match chars.next() {
                Some('"') => value.push('"'),
                Some('\\') => value.push('\\'),
                _ => return Err(syntax(line, "unsupported escape in string")),
            },
            Some(c) if c.is_control() => {
                return Err(syntax(line, "control character in string"));
            }
            Some(c) => value.push(c),
        }
    }
}

// -- Parser -----------------------------------------------------------------------

// Token-Strom mit Vorschau.
type Tokens<'a> = std::iter::Peekable<std::slice::Iter<'a, Lexed>>;

// Baut das Dokument aus dem Token-Strom.
fn parse_document(tokens: &[Lexed]) -> WardenResult<Document> {
    let mut document = Document::new();
    let mut tables: Vec<String> = Vec::new();
    let mut table = String::new();
    let mut stream: Tokens<'_> = tokens.iter().peekable();
    while let Some(lexed) = stream.next() {
        match &lexed.token {
            Token::Newline => {}
            Token::LBracket => {
                let name = match stream.next() {
                    Some(Lexed {
                        token: Token::Word(name),
                        ..
                    }) => name.clone(),
                    _ => return Err(syntax(lexed.line, "expected a table name")),
                };
                expect_token(&mut stream, &Token::RBracket, lexed.line)?;
                expect_line_end(&mut stream, lexed.line)?;
                // Auch eine leere, vertippte Tabelle (`[prooof]`) ist ein
                // Fehler, nicht nur eine mit unbekannten Schlüsseln.
                if !KNOWN_TABLES.contains(&name.as_str()) {
                    return Err(syntax(lexed.line, "unknown table"));
                }
                if tables.contains(&name) {
                    return Err(syntax(lexed.line, "table defined twice"));
                }
                tables.push(name.clone());
                table = name;
            }
            Token::Word(key) => {
                expect_token(&mut stream, &Token::Equals, lexed.line)?;
                let value = parse_value(&mut stream, lexed.line)?;
                expect_line_end(&mut stream, lexed.line)?;
                let previous = document.insert(
                    (table.clone(), key.clone()),
                    Entry {
                        value,
                        line: lexed.line,
                    },
                );
                if previous.is_some() {
                    return Err(syntax(lexed.line, "key defined twice"));
                }
            }
            _ => return Err(syntax(lexed.line, "expected a key or a table header")),
        }
    }
    Ok(document)
}

// Erwartet genau `wanted` als nächstes Token.
fn expect_token(stream: &mut Tokens<'_>, wanted: &Token, line: usize) -> WardenResult<()> {
    match stream.next() {
        Some(lexed) if lexed.token == *wanted => Ok(()),
        Some(lexed) => Err(syntax(lexed.line, "unexpected token")),
        None => Err(syntax(line, "unexpected end of file")),
    }
}

// Erwartet Zeilenende oder Dateiende.
fn expect_line_end(stream: &mut Tokens<'_>, line: usize) -> WardenResult<()> {
    match stream.next() {
        None
        | Some(Lexed {
            token: Token::Newline,
            ..
        }) => Ok(()),
        Some(lexed) => Err(syntax(lexed.line.max(line), "expected the end of the line")),
    }
}

// Überspringt Zeilenumbrüche (nur innerhalb eines Arrays erlaubt).
fn skip_newlines(stream: &mut Tokens<'_>) {
    while stream
        .peek()
        .is_some_and(|lexed| lexed.token == Token::Newline)
    {
        stream.next();
    }
}

// Liest einen Wert nach `=`.
fn parse_value(stream: &mut Tokens<'_>, line: usize) -> WardenResult<Value> {
    match stream.next() {
        Some(Lexed {
            token: Token::Str(value),
            ..
        }) => Ok(Value::Str(value.clone())),
        Some(Lexed {
            token: Token::Word(word),
            line,
        }) => parse_word(word, *line),
        Some(Lexed {
            token: Token::LBracket,
            line,
        }) => parse_string_array(stream, *line),
        Some(lexed) => Err(syntax(lexed.line, "expected a value")),
        None => Err(syntax(line, "unexpected end of file")),
    }
}

// Wort als nicht negative Dezimalzahl (ohne führende Nullen).
fn parse_word(word: &str, line: usize) -> WardenResult<Value> {
    let digits_only = !word.is_empty() && word.bytes().all(|b| b.is_ascii_digit());
    let leading_zero = word.len() > 1 && word.starts_with('0');
    if !digits_only || leading_zero {
        return Err(syntax(line, "expected a string, a number or an array"));
    }
    word.parse::<u64>()
        .map(Value::Int)
        .map_err(|_| syntax(line, "number is out of range"))
}

// Liest ein String-Array nach `[` (mehrzeilig, optionales Schlusskomma).
fn parse_string_array(stream: &mut Tokens<'_>, line: usize) -> WardenResult<Value> {
    let mut values = Vec::new();
    loop {
        skip_newlines(stream);
        match stream.next() {
            Some(Lexed {
                token: Token::RBracket,
                ..
            }) => return Ok(Value::StrArray(values)),
            Some(Lexed {
                token: Token::Str(value),
                ..
            }) => values.push(value.clone()),
            Some(lexed) => return Err(syntax(lexed.line, "arrays may only contain strings")),
            None => return Err(syntax(line, "unterminated array")),
        }
        skip_newlines(stream);
        match stream.next() {
            Some(Lexed {
                token: Token::Comma,
                ..
            }) => {}
            Some(Lexed {
                token: Token::RBracket,
                ..
            }) => return Ok(Value::StrArray(values)),
            Some(lexed) => return Err(syntax(lexed.line, "expected , or ] in an array")),
            None => return Err(syntax(line, "unterminated array")),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    use std::path::PathBuf;

    use super::{
        DEFAULT_PROOF_KEY_DIR, EscalatorIdentity, load_warden_config, parse_warden_config,
    };
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::trust::TrustRules;
    use harw_types::CgroupId;

    const FULL: &str = r#"# Warden policy
schema = 1
escalator_user = "harw-escalator"
proof_key_dir = "/etc/harw-dod/keys-2026"
allowed_cgroup_prefixes = [
    "harw.slice",
    "user.slice/user-1000.slice/user@1000.service/app.slice", # Jobs des Nutzers
]
allowed_actions = ["freeze-cgroup", "thaw-cgroup", "kill-process-tree"]
deny = ["harw.slice/protected-*.scope/**"]

[proof]
max_ttl_secs = 90
max_clock_skew_secs = 10
"#;

    fn syntax_line(result: crate::error::WardenResult<super::WardenConfig>) -> TestResult<usize> {
        match result {
            Err(WardenError::ConfigSyntax { line, .. }) => Ok(line),
            other => Err(TestError::Unexpected(format!(
                "expected ConfigSyntax, got {other:?}"
            ))),
        }
    }

    fn is_invalid(result: &crate::error::WardenResult<super::WardenConfig>) -> bool {
        matches!(result, Err(WardenError::ConfigInvalid { .. }))
    }

    #[test]
    fn test_full_config_parses_every_field() -> TestResult {
        let config = parse_warden_config(FULL).map_err(ctx("full config parses"))?;
        assert_eq!(
            config.escalator,
            EscalatorIdentity::User("harw-escalator".to_owned())
        );
        assert_eq!(
            config.proof_key_dir,
            PathBuf::from("/etc/harw-dod/keys-2026")
        );
        assert_eq!(
            config.policy.actions().kinds(),
            ["freeze-cgroup", "kill-process-tree", "thaw-cgroup"]
        );
        assert_eq!(config.policy.proof().max_ttl_secs, 90);
        assert_eq!(config.policy.proof().max_clock_skew_secs, 10);
        assert_eq!(config.policy.proof().allowed_cgroup_prefixes.len(), 2);
        let protected =
            CgroupId::try_from_str("harw.slice/protected-1.scope").map_err(ctx("cgroup id"))?;
        assert!(config.policy.deny().denies(&protected));
        Ok(())
    }

    #[test]
    fn test_minimal_config_uses_defaults() -> TestResult {
        let config = parse_warden_config("schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]")
            .map_err(ctx("minimal config parses"))?;
        assert_eq!(
            config.escalator,
            EscalatorIdentity::User("harw-escalator".to_owned())
        );
        assert_eq!(config.proof_key_dir, PathBuf::from(DEFAULT_PROOF_KEY_DIR));
        assert_eq!(
            config.policy.actions().kinds(),
            ["freeze-cgroup", "thaw-cgroup"]
        );
        assert_eq!(config.policy.proof().max_ttl_secs, 60);
        assert_eq!(config.policy.proof().max_clock_skew_secs, 5);
        Ok(())
    }

    #[test]
    fn test_numeric_escalator_uid_and_exclusivity() -> TestResult {
        let config = parse_warden_config(
            "schema = 1\nescalator_uid = 987\nallowed_cgroup_prefixes = [\"harw.slice\"]\n",
        )
        .map_err(ctx("uid config parses"))?;
        assert_eq!(config.escalator, EscalatorIdentity::Uid(987));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nescalator_uid = 0\nallowed_cgroup_prefixes = [\"harw.slice\"]\n"
        )));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nescalator_uid = 1\nescalator_user = \"x\"\nallowed_cgroup_prefixes = [\"harw.slice\"]\n"
        )));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nescalator_user = \"-bad\"\nallowed_cgroup_prefixes = [\"harw.slice\"]\n"
        )));
        Ok(())
    }

    #[test]
    fn test_unknown_duplicate_and_mistyped_keys_are_syntax_errors() -> TestResult {
        assert_eq!(
            syntax_line(parse_warden_config(
                "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\nsurprise = 1\n"
            ))?,
            3
        );
        assert_eq!(
            syntax_line(parse_warden_config("schema = 1\nschema = 1\n"))?,
            2
        );
        assert_eq!(
            syntax_line(parse_warden_config(
                "schema = \"1\"\nallowed_cgroup_prefixes = [\"harw.slice\"]\n"
            ))?,
            1
        );
        assert_eq!(
            syntax_line(parse_warden_config(
                "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\n[proof]\n[proof]\n"
            ))?,
            4
        );
        assert_eq!(
            syntax_line(parse_warden_config(
                "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\n[other]\nx = 1\n"
            ))?,
            3
        );
        Ok(())
    }

    #[test]
    fn test_unknown_table_without_keys_is_a_syntax_error() -> TestResult {
        // Ein vertippter, leerer Tabellenkopf fiele sonst stillschweigend durch.
        assert_eq!(
            syntax_line(parse_warden_config(
                "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\n[prooof]\n"
            ))?,
            3
        );
        // Die bekannte Tabelle darf leer sein.
        let config = parse_warden_config(
            "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\n[proof]\n",
        )
        .map_err(ctx("empty proof table parses"))?;
        assert_eq!(config.policy.proof().max_ttl_secs, 60);
        Ok(())
    }

    #[test]
    fn test_outside_the_subset_is_rejected() {
        for text in [
            "schema = -1\n",
            "schema = 0x1\n",
            "schema = 01\n",
            "schema = 1_0\n",
            "schema = 1.0\n",
            "a.b = 1\n",
            "name = 'single'\n",
            "name = \"tab\\t\"\n",
            "name = \"\"\"multi\"\"\"\n",
            "name = \"unterminated\n",
            "list = [1, 2]\n",
            "list = [\"a\" \"b\"]\n",
            "[[array.table]]\n",
            "key = { inline = 1 }\n",
            "schema = 1 schema = 1\n",
            "schema = 1\r",
            "flag = true\n",
        ] {
            assert!(
                matches!(
                    parse_warden_config(text),
                    Err(WardenError::ConfigSyntax { .. })
                ),
                "{text:?} must be rejected"
            );
        }
    }

    #[test]
    fn test_semantic_errors_are_rejected() {
        assert!(is_invalid(&parse_warden_config("schema = 2\n")));
        assert!(is_invalid(&parse_warden_config(
            "allowed_cgroup_prefixes = [\"harw.slice\"]\n"
        )));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nallowed_cgroup_prefixes = []\n"
        )));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\nallowed_actions = [\"release-cgroup\"]\n"
        )));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\ndeny = [\"../x\"]\n"
        )));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\nproof_key_dir = \"relative/keys\"\n"
        )));
        assert!(is_invalid(&parse_warden_config(
            "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\nproof_key_dir = \"/etc/../tmp\"\n"
        )));
        assert!(matches!(
            parse_warden_config(
                "schema = 1\nallowed_cgroup_prefixes = [\"harw.slice\"]\n[proof]\nmax_ttl_secs = 600\n"
            ),
            Err(WardenError::Proof(_))
        ));
    }

    #[test]
    fn test_crlf_line_breaks_and_escapes_are_accepted() -> TestResult {
        let config = parse_warden_config(
            "schema = 1\r\nallowed_cgroup_prefixes = [\"harw.slice\",]\r\ndeny = [\"harw.slice/a\\\\b\"]\r\n",
        )
        .map_err(ctx("crlf config parses"))?;
        assert_eq!(
            config.policy.proof().allowed_cgroup_prefixes,
            ["harw.slice"]
        );
        Ok(())
    }

    #[test]
    fn test_load_applies_trust_checks() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let owner = std::fs::metadata(dir.path())
            .map_err(ctx("metadata"))?
            .uid();
        let rules = TrustRules::owned_by(owner);
        let path = dir.path().join("warden.toml");
        std::fs::write(&path, FULL).map_err(ctx("write"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .map_err(ctx("chmod"))?;
        assert!(load_warden_config(&path, rules).is_ok());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .map_err(ctx("chmod world writable"))?;
        assert!(matches!(
            load_warden_config(&path, rules),
            Err(WardenError::UntrustedFile { .. })
        ));
        Ok(())
    }
}
