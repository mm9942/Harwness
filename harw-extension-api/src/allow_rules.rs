//! Freigaberegeln (Allow/Deny) für Werkzeugaufrufe.
//!
//! # Verantwortlichkeit
//! Dieses Modul besitzt die Frage „gibt es eine hinterlegte Regel, die diesen
//! konkreten Aufruf ohne Rückfrage erlaubt oder ausdrücklich verbietet?". Es
//! entscheidet nicht über den Rest — ein Aufruf ohne passende Regel liefert
//! `None` und die eigentliche Freigabepolitik (siehe `harw-runtime`) fragt
//! dann nach dem [`crate::approval_mode::ApprovalMode`].
//!
//! # Schlüsseltypen
//! - [`RuleDecision`] — `Allow` oder `Deny`
//! - [`RuleScope`] — Lebensdauer der Regel (`Session`, `Project`, `Global`)
//! - [`ApprovalRule`] — eine einzelne Regel
//! - [`AllowRuleSet`] — die geteilte, klonbare Sammlung aller Regeln
//! - [`derive_shell_rule`] — leitet aus einem tatsächlich ausgeführten Befehl
//!   einen konservativen Regel-Vorschlag ab (für „diesen Befehl immer
//!   erlauben"-Angebote in der UI)
//!
//! # Nebenläufigkeit
//! [`AllowRuleSet`] trägt ihre Regeln in einem `Arc<RwLock<Vec<ApprovalRule>>>`
//! wie [`crate::approval_mode::ApprovalModeCell`] ihren Modus: viele Klone
//! teilen sich dieselbe Liste, viele gleichzeitige Leser, ein Schreiber.
//! Anders als dort ist [`AllowRuleSet::evaluate`] jedoch eine
//! sicherheitsrelevante Entscheidung: ein vergifteter Lock (ein anderer
//! Thread ist mit der Sperre paniert, der Zustand der Liste ist damit nicht
//! mehr vertrauenswürdig) führt hier **nicht** zum zuletzt bekannten Wert,
//! sondern schließt sofort mit [`RuleDecision::Deny`] — fail closed. Die rein
//! verwaltenden Methoden ([`AllowRuleSet::add`], [`AllowRuleSet::remove`],
//! [`AllowRuleSet::snapshot`]) entnehmen einem vergifteten Lock dagegen wie
//! `ApprovalModeCell` den zuletzt geschriebenen Zustand über `into_inner`,
//! weil dort keine Freigabeentscheidung getroffen wird.
//!
//! # Fehler
//! Das Modul erzeugt keine Fehler. Ein Aufruf ohne passende Regel liefert
//! `None`, kein `Result`.
//!
//! # Beispiele
//! ```rust
//! use harw_extension_api::allow_rules::{AllowRuleSet, ApprovalRule, RuleDecision, RuleScope};
//! use serde_json::json;
//!
//! let rules = AllowRuleSet::new();
//! rules.add(ApprovalRule {
//!     tool: "shell.exec".to_owned(),
//!     pattern: Some("git status".to_owned()),
//!     decision: RuleDecision::Allow,
//!     scope: RuleScope::Session,
//! });
//!
//! assert_eq!(
//!     rules.evaluate("shell.exec", &json!({"command": "git status --short"})),
//!     Some(RuleDecision::Allow)
//! );
//! ```

use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// Ergebnis, das eine [`ApprovalRule`] für einen passenden Aufruf trägt.
///
/// # Beschreibung
/// `Deny` gewinnt immer über `Allow`, auch wenn eine andere passende Regel
/// aus einem höher priorisierten [`RuleScope`] stammt — siehe
/// [`AllowRuleSet::evaluate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleDecision {
    /// Der Aufruf darf ohne Rückfrage laufen.
    Allow,
    /// Der Aufruf wird ohne Rückfrage verweigert.
    Deny,
}

/// Lebensdauer, aus der eine [`ApprovalRule`] stammt.
///
/// # Beschreibung
/// Rein informativ für dieses Modul selbst — [`AllowRuleSet::evaluate`]
/// gewichtet keinen Scope höher als einen anderen, „Deny gewinnt über alle
/// Scopes hinweg" (siehe Contract §2). Welche Regeln in welchem Scope
/// gespeichert und geladen werden, entscheidet der Aufrufer (`harw-runtime`,
/// `harw-ops`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleScope {
    /// Lebt nur im Speicher, endet mit der Session.
    Session,
    /// Dauerhaft pro Projekt.
    Project,
    /// Dauerhaft für den User.
    Global,
}

/// Eine einzelne Freigaberegel für ein Werkzeug.
///
/// # Beschreibung
/// - `tool`: exakter Werkzeugname (`"shell.exec"`) oder Präfix-Glob mit
///   abschließendem `*` (`"fs.*"`).
/// - `pattern`: `None` trifft auf jeden Aufruf dieses Werkzeugs zu. `Some`
///   wird je nach Werkzeug unterschiedlich ausgewertet — siehe
///   [`AllowRuleSet::evaluate`].
/// - `decision`: was bei Treffer gilt.
/// - `scope`: woher die Regel stammt (siehe [`RuleScope`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRule {
    /// Werkzeugname, exakt oder als Präfix-Glob mit abschließendem `*`.
    pub tool: String,
    /// Optionales Muster; Bedeutung hängt vom Werkzeug ab (siehe Modul-Doku).
    pub pattern: Option<String>,
    /// Ob ein Treffer erlaubt oder verweigert.
    pub decision: RuleDecision,
    /// Lebensdauer, aus der diese Regel stammt.
    pub scope: RuleScope,
}

/// Geteilte, klonbare Sammlung von [`ApprovalRule`]n.
///
/// # Verantwortlichkeit
/// `AllowRuleSet` trägt die Regeln selbst: mehrere Klone teilen sich dieselbe
/// Liste (nützlich, wenn z. B. eine Sitzung und ihre Kind-Sitzungen dieselbe
/// Regelmenge sehen sollen). Es gibt keine `detached()`-Methode wie bei
/// [`crate::approval_mode::ApprovalModeCell`], weil Regeln je nach Scope aus
/// unterschiedlichen, vom Aufrufer verwalteten Quellen zusammengeführt
/// werden — das Zusammenführen ist Sache des Aufrufers, nicht dieses Typs.
///
/// # Nebenläufigkeit
/// Siehe die Modul-Doku für das Vergiftungsverhalten von
/// [`Self::evaluate`] gegenüber den übrigen Methoden.
#[derive(Clone)]
pub struct AllowRuleSet(Arc<RwLock<Vec<ApprovalRule>>>);

impl AllowRuleSet {
    /// Erzeugt eine neue, leere Regelmenge.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Vec::new())))
    }

    /// Erzeugt eine Regelmenge aus bereits vorhandenen Regeln, etwa beim
    /// Laden aus `Global`- und `Project`-Konfiguration.
    ///
    /// # Arguments
    /// - `rules` (`Vec<ApprovalRule>`): die anfänglichen Regeln, in der
    ///   gegebenen Reihenfolge.
    #[must_use]
    pub fn from_rules(rules: Vec<ApprovalRule>) -> Self {
        Self(Arc::new(RwLock::new(rules)))
    }

    /// Fügt eine Regel hinzu, sofern sie nicht bereits (feldweise identisch)
    /// vorhanden ist.
    ///
    /// # Arguments
    /// - `rule` (`ApprovalRule`): die hinzuzufügende Regel.
    ///
    /// # Rückgabe
    /// `true`, wenn die Regel neu war und angehängt wurde; `false`, wenn eine
    /// feldweise identische Regel bereits existierte (keine Dopplung).
    ///
    /// # Nebenläufigkeit
    /// Ein vergifteter Lock wird wie bei `ApprovalModeCell` über
    /// `into_inner` aufgelöst statt weiterzureichen — hier wird nur die
    /// Liste verwaltet, keine Freigabeentscheidung getroffen.
    pub fn add(&self, rule: ApprovalRule) -> bool {
        let mut guard = match self.0.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if guard.contains(&rule) {
            false
        } else {
            guard.push(rule);
            true
        }
    }

    /// Entfernt die Regel am gegebenen Index.
    ///
    /// # Arguments
    /// - `index` (`usize`): Position in der aktuellen Reihenfolge (siehe
    ///   [`Self::snapshot`]).
    ///
    /// # Rückgabe
    /// Die entfernte Regel, oder `None` bei einem Index außerhalb der Liste.
    ///
    /// # Nebenläufigkeit
    /// Siehe [`Self::add`].
    pub fn remove(&self, index: usize) -> Option<ApprovalRule> {
        let mut guard = match self.0.write() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if index < guard.len() { Some(guard.remove(index)) } else { None }
    }

    /// Liefert eine Kopie aller aktuell gespeicherten Regeln.
    ///
    /// # Rückgabe
    /// Die Regeln in ihrer aktuellen Reihenfolge.
    ///
    /// # Nebenläufigkeit
    /// Siehe [`Self::add`].
    #[must_use]
    pub fn snapshot(&self) -> Vec<ApprovalRule> {
        match self.0.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Wertet alle Regeln gegen einen konkreten Werkzeugaufruf aus.
    ///
    /// # Beschreibung
    /// Sammelt alle Regeln, deren `tool` auf `tool` passt (exakt oder als
    /// `prefix*`-Glob) und deren `pattern` auf `arguments` passt (siehe
    /// unten). Trifft mindestens eine passende Regel mit
    /// [`RuleDecision::Deny`] zu, gewinnt `Deny` — unabhängig davon, wie
    /// viele passende `Allow`-Regeln es daneben gibt und aus welchem
    /// [`RuleScope`] sie stammen. Andernfalls gewinnt `Allow`, wenn
    /// mindestens eine passende `Allow`-Regel existiert. Passt keine Regel,
    /// liefert die Methode `None` — die aufrufende Politik muss dann nach
    /// [`crate::approval_mode::ApprovalMode`] entscheiden.
    ///
    /// Musterauswertung je nach `tool`:
    /// - `pattern: None` passt auf jeden Aufruf dieses Werkzeugs.
    /// - `tool == "shell.exec"`: `pattern` ist ein Befehls-Präfix, das auf
    ///   ganzen Whitespace-Tokens verglichen wird (`"git status"` passt auf
    ///   `"git status --short"`, nicht auf `"git statusx"`). Enthält der
    ///   Befehl eines der Metazeichen `; && || | `$( > < \n` oder ein
    ///   token-endständiges `&`, passt er **niemals** auf eine `Allow`-Regel;
    ///   eine `Deny`-Regel prüft in diesem Fall nur das erste, ungefährliche
    ///   Teilstück des Befehls.
    /// - `tool` beginnt mit `"fs."`: `pattern` ist ein Datei-Glob (`*`
    ///   innerhalb eines Pfadsegments, `**` über Segmente hinweg, `?` für ein
    ///   einzelnes Zeichen), ausgewertet gegen das Argument `path`. Enthält
    ///   `path` eine `..`-Komponente, kann die Regel nicht als `Allow`
    ///   treffen (eine `Deny`-Regel greift trotzdem).
    /// - jedes andere Werkzeug: nur `pattern: None`-Regeln treffen zu.
    ///
    /// # Arguments
    /// - `tool` (`&str`): der aufgerufene Werkzeugname.
    /// - `arguments` (`&serde_json::Value`): die Argumente des Aufrufs.
    ///
    /// # Rückgabe
    /// `Some(RuleDecision::Deny)`, `Some(RuleDecision::Allow)` oder `None`.
    ///
    /// # Nebenläufigkeit
    /// Ein vergifteter Lock schließt sofort mit `Some(RuleDecision::Deny)` —
    /// fail closed, siehe Modul-Doku. Das unterscheidet diese Methode
    /// bewusst von `ApprovalModeCell::get`.
    #[must_use]
    pub fn evaluate(&self, tool: &str, arguments: &serde_json::Value) -> Option<RuleDecision> {
        let guard = match self.0.read() {
            Ok(guard) => guard,
            Err(_poisoned) => return Some(RuleDecision::Deny),
        };

        let mut any_allow = false;
        for rule in guard.iter() {
            if !tool_matches(&rule.tool, tool) {
                continue;
            }
            if !rule_matches_call(rule, tool, arguments) {
                continue;
            }
            match rule.decision {
                RuleDecision::Deny => return Some(RuleDecision::Deny),
                RuleDecision::Allow => any_allow = true,
            }
        }

        if any_allow { Some(RuleDecision::Allow) } else { None }
    }
}

impl std::fmt::Debug for AllowRuleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllowRuleSet").field("rules", &self.snapshot()).finish()
    }
}

impl Default for AllowRuleSet {
    /// Startet mit einer leeren Regelmenge.
    fn default() -> Self {
        Self::new()
    }
}

/// Prüft, ob ein Regel-Werkzeugname auf einen aufgerufenen Werkzeugnamen
/// passt.
///
/// # Beschreibung
/// Exakte Übereinstimmung, oder `rule_tool` endet auf `*` und `tool` beginnt
/// mit dem Präfix davor (z. B. `"fs.*"` passt auf `"fs.read"`).
fn tool_matches(rule_tool: &str, tool: &str) -> bool {
    match rule_tool.strip_suffix('*') {
        Some(prefix) => tool.starts_with(prefix),
        None => rule_tool == tool,
    }
}

/// Prüft, ob das `pattern` einer Regel auf den konkreten Aufruf passt.
///
/// Siehe [`AllowRuleSet::evaluate`] für die vollständige Beschreibung je
/// Werkzeug-Art.
fn rule_matches_call(rule: &ApprovalRule, tool: &str, arguments: &serde_json::Value) -> bool {
    let Some(pattern) = &rule.pattern else {
        return true;
    };

    if tool == "shell.exec" {
        match arguments.get("command").and_then(|v| v.as_str()) {
            Some(command) => shell_pattern_matches(pattern, command, rule.decision),
            None => false,
        }
    } else if tool.starts_with("fs.") {
        match arguments.get("path").and_then(|v| v.as_str()) {
            Some(path) => {
                if rule.decision == RuleDecision::Allow && has_dotdot_component(path) {
                    false
                } else {
                    glob_match_path(pattern, path)
                }
            }
            None => false,
        }
    } else {
        // Andere Werkzeuge: nur pattern=None-Regeln treffen zu, siehe oben.
        false
    }
}

/// Zeichen bzw. Zeichenfolgen, die einen Shell-Befehl als potenziell
/// zusammengesetzt (mehrere Kommandos) markieren.
const DANGEROUS_SHELL_MARKERS: [&str; 9] = [";", "&&", "||", "|", "`", "$(", ">", "<", "\n"];

/// Prüft, ob ein Befehl eines der gefährlichen Shell-Metazeichen enthält,
/// oder ein Token token-endständig mit `&` abschließt (Hintergrundjob).
fn has_dangerous_shell_chars(command: &str) -> bool {
    if DANGEROUS_SHELL_MARKERS.iter().any(|marker| command.contains(marker)) {
        return true;
    }
    command.split_whitespace().any(|token| token.ends_with('&'))
}

/// Liefert das erste, ungefährliche Teilstück eines Befehls (vor dem ersten
/// gefährlichen Metazeichen), rechts getrimmt.
fn first_safe_segment(command: &str) -> &str {
    let mut cut = command.len();
    let bytes = command.as_bytes();
    for (i, c) in command.char_indices() {
        let hits_marker = matches!(c, ';' | '&' | '|' | '`' | '>' | '<' | '\n')
            || (c == '$' && bytes.get(i + 1) == Some(&b'('));
        if hits_marker {
            cut = i;
            break;
        }
    }
    command[..cut].trim_end()
}

/// Prüft ein `shell.exec`-Muster gegen einen tatsächlichen Befehl.
///
/// # Beschreibung
/// `pattern` wird auf Whitespace-Tokens zerlegt (ein abschließendes `*` als
/// eigenes Token wird verworfen — Präfix-Vergleich erlaubt ohnehin beliebig
/// viele weitere Tokens). Ist der Befehl gefährlich (siehe
/// [`has_dangerous_shell_chars`]), trifft eine `Allow`-Regel nie zu; eine
/// `Deny`-Regel prüft nur das erste ungefährliche Teilstück.
fn shell_pattern_matches(pattern: &str, command: &str, decision: RuleDecision) -> bool {
    let command = command.trim_start();
    let dangerous = has_dangerous_shell_chars(command);

    if dangerous && decision == RuleDecision::Allow {
        return false;
    }

    let segment = if dangerous { first_safe_segment(command) } else { command };

    let mut pattern_tokens: Vec<&str> = pattern.split_whitespace().collect();
    if pattern_tokens.last() == Some(&"*") {
        pattern_tokens.pop();
    }
    if pattern_tokens.is_empty() {
        return true;
    }

    let segment_tokens: Vec<&str> = segment.split_whitespace().collect();
    if segment_tokens.len() < pattern_tokens.len() {
        return false;
    }

    segment_tokens[..pattern_tokens.len()] == pattern_tokens[..]
}

/// Prüft, ob ein Pfad eine `..`-Komponente enthält.
fn has_dotdot_component(path: &str) -> bool {
    path.split('/').any(|segment| segment == "..")
}

/// Prüft einen Pfad-Glob (`*` innerhalb eines Segments, `**` über Segmente
/// hinweg, `?` für ein einzelnes Zeichen) gegen einen Pfad.
///
/// Beide Seiten werden an `/` in Segmente zerlegt und segmentweise
/// verglichen; `**` konsumiert null oder mehr ganze Segmente.
fn glob_match_path(pattern: &str, path: &str) -> bool {
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    match_segments(&pattern_segments, &path_segments)
}

/// Vergleicht Pfadsegmente rekursiv gegen Musterssegmente, mit `**` als
/// Platzhalter für null oder mehr ganze Segmente.
fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(&"**") => {
            let rest = &pattern[1..];
            if rest.is_empty() {
                return true;
            }
            (0..=path.len()).any(|skip| match_segments(rest, &path[skip..]))
        }
        Some(&segment_pattern) => match path.first() {
            Some(&segment) if segment_match(segment_pattern, segment) => {
                match_segments(&pattern[1..], &path[1..])
            }
            _ => false,
        },
    }
}

/// Vergleicht ein einzelnes Pfadsegment gegen ein Muster mit `*` (null oder
/// mehr Zeichen) und `?` (genau ein Zeichen).
fn segment_match(pattern: &str, segment: &str) -> bool {
    wildcard_match(pattern.as_bytes(), segment.as_bytes())
}

/// Klassischer rekursiver Wildcard-Abgleich (`*`, `?`) auf Byte-Ebene.
fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    match (pattern.first(), text.first()) {
        (None, None) => true,
        (Some(b'*'), _) => {
            wildcard_match(&pattern[1..], text)
                || (!text.is_empty() && wildcard_match(pattern, &text[1..]))
        }
        (Some(b'?'), Some(_)) => wildcard_match(&pattern[1..], &text[1..]),
        (Some(p), Some(t)) if p == t => wildcard_match(&pattern[1..], &text[1..]),
        _ => false,
    }
}

/// Denylist breiter Interpreter/Wrapper, für die `derive_shell_rule` keinen
/// Vorschlag ableitet (das erste Token allein wäre zu weitreichend, ein
/// Zwei-Token-Vorschlag oft irreführend, weil das zweite Token beliebigen
/// Code enthalten kann).
const BROAD_INTERPRETER_DENYLIST: [&str; 19] = [
    "sh", "bash", "zsh", "fish", "env", "sudo", "doas", "xargs", "eval", "exec", "python",
    "python3", "node", "perl", "ruby", "nohup", "time", "timeout", "nice",
];

/// Leitet aus einem tatsächlich ausgeführten Shell-Befehl einen
/// konservativen Regel-Vorschlag ab (für „diesen Befehl immer erlauben" in
/// der UI).
///
/// # Beschreibung
/// Liefert die ersten zwei Tokens des Befehls (oder nur das erste, wenn der
/// Befehl nur ein Token hat oder das zweite Token mit `-` beginnt — ein
/// Flag ist Teil derselben Aufrufform, kein eigenständiges Unterkommando).
/// Liefert `None`, wenn der Befehl eines der in
/// [`has_dangerous_shell_chars`] geprüften Metazeichen enthält, oder das
/// erste Token in [`BROAD_INTERPRETER_DENYLIST`] steht — ein Vorschlag wie
/// `"bash"` oder `"python3"` allein wäre faktisch eine Freigabe für beliebig
/// weiteren Code.
///
/// # Arguments
/// - `command` (`&str`): der ausgeführte Befehl, wie er dem `shell.exec`-
///   Werkzeug übergeben wurde.
///
/// # Rückgabe
/// `Some(vorschlag)` als Muster für [`ApprovalRule::pattern`], oder `None`,
/// wenn kein sicherer Vorschlag ableitbar ist.
#[must_use]
pub fn derive_shell_rule(command: &str) -> Option<String> {
    if has_dangerous_shell_chars(command) {
        return None;
    }

    let tokens: Vec<&str> = command.split_whitespace().collect();
    let first = *tokens.first()?;

    if BROAD_INTERPRETER_DENYLIST.contains(&first) {
        return None;
    }

    match tokens.get(1) {
        Some(second) if !second.starts_with('-') => Some(format!("{first} {second}")),
        _ => Some(first.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AllowRuleSet, ApprovalRule, RuleDecision, RuleScope, derive_shell_rule, glob_match_path,
    };
    use serde_json::json;

    fn rule(tool: &str, pattern: Option<&str>, decision: RuleDecision, scope: RuleScope) -> ApprovalRule {
        ApprovalRule {
            tool: tool.to_owned(),
            pattern: pattern.map(str::to_owned),
            decision,
            scope,
        }
    }

    #[test]
    fn test_evaluate_shell_dangerous_command_never_allowed_by_prefix_rule() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "shell.exec",
            Some("git status"),
            RuleDecision::Allow,
            RuleScope::Session,
        ));

        let result = rules.evaluate("shell.exec", &json!({"command": "git status; rm -rf /"}));

        assert_eq!(result, None, "a dangerous compound command must never be allowed by a prefix rule");
    }

    #[test]
    fn test_evaluate_shell_matches_on_whole_token_boundary() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "shell.exec",
            Some("git status"),
            RuleDecision::Allow,
            RuleScope::Session,
        ));

        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "git status --short"})),
            Some(RuleDecision::Allow),
            "extra tokens after the pattern's tokens are allowed"
        );
        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "git statusx"})),
            None,
            "a token must match in full, not as a substring prefix"
        );
    }

    #[test]
    fn test_evaluate_fs_tool_glob_matches_path() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "fs.*",
            Some("/home/user/project/*"),
            RuleDecision::Allow,
            RuleScope::Project,
        ));

        assert_eq!(
            rules.evaluate("fs.read", &json!({"path": "/home/user/project/file.txt"})),
            Some(RuleDecision::Allow)
        );
        assert_eq!(
            rules.evaluate("fs.read", &json!({"path": "/home/user/other/file.txt"})),
            None
        );
    }

    #[test]
    fn test_glob_double_star_matches_across_segments() {
        assert!(glob_match_path("/home/user/**/*.rs", "/home/user/a/b/c/main.rs"));
        assert!(glob_match_path("/home/user/**/*.rs", "/home/user/main.rs"));
        assert!(!glob_match_path("/home/user/**/*.rs", "/home/user/main.txt"));
    }

    #[test]
    fn test_evaluate_dotdot_path_never_matches_allow() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "fs.*",
            Some("/home/user/project/**"),
            RuleDecision::Allow,
            RuleScope::Project,
        ));

        assert_eq!(
            rules.evaluate("fs.read", &json!({"path": "/home/user/project/../secret"})),
            None
        );
    }

    #[test]
    fn test_evaluate_deny_beats_allow_across_scopes() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "shell.exec",
            Some("git push"),
            RuleDecision::Allow,
            RuleScope::Global,
        ));
        rules.add(rule(
            "shell.exec",
            Some("git push"),
            RuleDecision::Deny,
            RuleScope::Session,
        ));

        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "git push origin main"})),
            Some(RuleDecision::Deny),
            "deny must win regardless of scope precedence"
        );
    }

    #[test]
    fn test_evaluate_poisoned_lock_returns_deny() {
        let rules = AllowRuleSet::new();
        rules.add(rule("shell.exec", None, RuleDecision::Allow, RuleScope::Session));

        let poison_rules = rules.clone();
        let handle = std::thread::spawn(move || {
            let _guard = poison_rules.0.write().unwrap();
            panic!("deliberately poisoning the lock for the test");
        });
        let _ = handle.join();

        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "anything"})),
            Some(RuleDecision::Deny),
            "a poisoned lock must fail closed to Deny"
        );
    }

    #[test]
    fn test_add_dedupes_identical_rules() {
        let rules = AllowRuleSet::new();
        let a = rule("shell.exec", Some("git status"), RuleDecision::Allow, RuleScope::Session);
        let b = rule("shell.exec", Some("git status"), RuleDecision::Allow, RuleScope::Session);

        assert!(rules.add(a));
        assert!(!rules.add(b));
        assert_eq!(rules.snapshot().len(), 1);
    }

    #[test]
    fn test_remove_returns_removed_rule_and_none_out_of_bounds() {
        let rules = AllowRuleSet::new();
        let a = rule("shell.exec", None, RuleDecision::Allow, RuleScope::Session);
        rules.add(a.clone());

        assert_eq!(rules.remove(0), Some(a));
        assert_eq!(rules.remove(0), None);
    }

    #[test]
    fn test_from_rules_seeds_initial_state() {
        let seed = vec![rule("shell.exec", None, RuleDecision::Allow, RuleScope::Global)];
        let rules = AllowRuleSet::from_rules(seed.clone());
        assert_eq!(rules.snapshot(), seed);
    }

    #[test]
    fn test_other_tool_only_matches_pattern_none_rules() {
        let rules = AllowRuleSet::new();
        rules.add(rule("agent.spawn", Some("anything"), RuleDecision::Allow, RuleScope::Session));
        rules.add(rule("agent.stop", None, RuleDecision::Allow, RuleScope::Session));

        assert_eq!(rules.evaluate("agent.spawn", &json!({})), None);
        assert_eq!(rules.evaluate("agent.stop", &json!({})), Some(RuleDecision::Allow));
    }

    #[test]
    fn test_derive_shell_rule_two_tokens_when_second_is_not_a_flag() {
        assert_eq!(derive_shell_rule("git status --short"), Some("git status".to_owned()));
    }

    #[test]
    fn test_derive_shell_rule_single_token_when_second_is_a_flag() {
        assert_eq!(derive_shell_rule("ls -la"), Some("ls".to_owned()));
    }

    #[test]
    fn test_derive_shell_rule_none_for_broad_interpreter() {
        assert_eq!(derive_shell_rule("bash -c x"), None);
    }

    #[test]
    fn test_derive_shell_rule_none_for_piped_command() {
        assert_eq!(derive_shell_rule("cat a | grep b"), None);
    }
}
