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
        if index < guard.len() {
            Some(guard.remove(index))
        } else {
            None
        }
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
    ///   Befehl eines der Metazeichen `` ; & | ` $( > < \n \r `` — `&` an jeder
    ///   Stelle, auch mitten im Wort (`git status&rm x`) —, passt er
    ///   **niemals** auf eine `Allow`-Regel. Eine `Deny`-Regel wird gegen
    ///   jedes Teilstück zwischen diesen Trennzeichen (zusätzlich `(`/`)`)
    ///   geprüft und trifft, sobald eines davon passt (`true; rm -rf ~`
    ///   trifft `Deny`-Regel `rm`).
    /// - `tool == "job.start"` (Plan R9, Teil F): wie `shell.exec`, über
    ///   `command` bzw. das gequotete `argv`; mit gesetztem `env` trifft
    ///   keine `Allow`-Regel.
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

        if any_allow {
            Some(RuleDecision::Allow)
        } else {
            None
        }
    }
}

impl std::fmt::Debug for AllowRuleSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AllowRuleSet")
            .field("rules", &self.snapshot())
            .finish()
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
    } else if tool == JOB_START_TOOL {
        // Plan R9, Teil F: `job.start` startet einen Shell-Befehl wie
        // `shell.exec` — dieselbe Präfix-Auswertung über `command` bzw. das
        // gequotete `argv`. Gesetzte Umgebungsvariablen (`env`) können das
        // Verhalten des Befehls ändern: eine `Allow`-Regel trifft dann nie.
        let has_env = arguments
            .get("env")
            .and_then(|v| v.as_array())
            .is_some_and(|env| !env.is_empty());
        if has_env && rule.decision == RuleDecision::Allow {
            return false;
        }
        match job_start_command(arguments) {
            Some(command) => shell_pattern_matches(pattern, &command, rule.decision),
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

/// Name des Job-Startwerkzeugs (`harw-tool-job`, Plan R9 Teil F); hier
/// dupliziert, weil `harw-tool-job` von diesem Crate abhängt.
const JOB_START_TOOL: &str = "job.start";

/// Befehlstext eines `job.start`-Aufrufs: `command` oder das mit
/// Shell-Quoting verbundene `argv` — wortgleich zu
/// `harw_tool_job::job_start_command_text` (ein Test in `harw-tool-job`
/// prüft die Übereinstimmung über [`AllowRuleSet::evaluate`]).
fn job_start_command(arguments: &serde_json::Value) -> Option<String> {
    let command = arguments
        .get("command")
        .and_then(|v| v.as_str())
        .filter(|command| !command.trim().is_empty());
    let argv: Option<Vec<&str>> = arguments
        .get("argv")
        .and_then(|v| v.as_array())
        .map(|words| {
            words
                .iter()
                .filter_map(|word| word.as_str())
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty());
    match (command, argv) {
        (Some(command), None) => Some(command.to_owned()),
        (None, Some(words)) => Some(
            words
                .into_iter()
                .map(quote_word)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => None,
    }
}

/// POSIX-Quoting eines argv-Worts (wie `harw_tool_job::shell_quote`).
fn quote_word(word: &str) -> String {
    let safe = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&byte));
    if safe {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// Zeichen bzw. Zeichenfolgen, die einen Shell-Befehl als potenziell
/// zusammengesetzt (mehrere Kommandos, Umleitung, Kommando-Ersetzung)
/// markieren. Jedes `&` zählt: in POSIX-sh ist ein einzelnes `&` auch mitten
/// in einem Wort ein Steueroperator (`git status&rm x`); `&&` und `||` sind
/// über `&` und `|` mit abgedeckt. `\r` trennt in sh nicht, dient aber nur
/// der Verschleierung und wird deshalb ebenfalls abgelehnt.
const DANGEROUS_SHELL_MARKERS: [&str; 9] = [";", "&", "|", "`", "$(", ">", "<", "\n", "\r"];

/// Prüft, ob ein Befehl eines der gefährlichen Shell-Metazeichen enthält.
fn has_dangerous_shell_chars(command: &str) -> bool {
    DANGEROUS_SHELL_MARKERS
        .iter()
        .any(|marker| command.contains(marker))
}

/// Zerlegt einen Befehl für die `Deny`-Prüfung in Teilstücke — an jedem
/// Zeichen aus [`DANGEROUS_SHELL_MARKERS`] sowie an `(`/`)`, damit auch
/// `$(…)`, `<(…)` und Subshells `(…)` als eigenes Teilstück geprüft werden.
fn shell_segments(command: &str) -> impl Iterator<Item = &str> {
    command.split(|c: char| {
        matches!(c, ';' | '&' | '|' | '`' | '>' | '<' | '(' | ')' | '\n' | '\r')
    })
}

/// Prüft ein `shell.exec`-Muster gegen einen tatsächlichen Befehl.
///
/// # Beschreibung
/// `pattern` wird auf Whitespace-Tokens zerlegt (ein abschließendes `*` als
/// eigenes Token wird verworfen — Präfix-Vergleich erlaubt ohnehin beliebig
/// viele weitere Tokens). Ist der Befehl gefährlich (siehe
/// [`has_dangerous_shell_chars`]), trifft eine `Allow`-Regel nie zu; eine
/// `Deny`-Regel trifft, sobald eines der [`shell_segments`] passt — im
/// Zweifel verweigern, nicht nur den Anfang des Befehls prüfen.
fn shell_pattern_matches(pattern: &str, command: &str, decision: RuleDecision) -> bool {
    let mut pattern_tokens: Vec<&str> = pattern.split_whitespace().collect();
    if pattern_tokens.last() == Some(&"*") {
        pattern_tokens.pop();
    }

    match decision {
        RuleDecision::Allow => {
            !has_dangerous_shell_chars(command) && tokens_match_prefix(&pattern_tokens, command)
        }
        RuleDecision::Deny => shell_segments(command)
            .any(|segment| tokens_match_prefix(&pattern_tokens, segment)),
    }
}

/// Prüft, ob die Whitespace-Tokens von `segment` mit `pattern_tokens`
/// beginnen; leere `pattern_tokens` passen auf alles.
fn tokens_match_prefix(pattern_tokens: &[&str], segment: &str) -> bool {
    // Runde 5, Teil E: ein an das letzte Muster-Token angehängtes `*`
    // (`"cargo test*"`) ist ein Präfix-Glob nur für dieses Token — die
    // Tokens davor müssen weiterhin exakt passen.
    let last = pattern_tokens.len().saturating_sub(1);
    let mut tokens = segment.split_whitespace();
    pattern_tokens.iter().enumerate().all(|(index, pattern_token)| {
        let Some(token) = tokens.next() else {
            return false;
        };
        match pattern_token.strip_suffix('*') {
            Some(prefix) if index == last && !prefix.is_empty() => token.starts_with(prefix),
            _ => *pattern_token == token,
        }
    })
}

/// Prüft, ob ein Pfad eine `..`-Komponente enthält.
fn has_dotdot_component(path: &str) -> bool {
    path.split('/').any(|segment| segment == "..")
}

/// Prüft einen Pfad-Glob (`*` innerhalb eines Segments, `**` über Segmente
/// hinweg, `?` für ein einzelnes Zeichen) gegen einen Pfad.
///
/// Beide Seiten werden an `/` in Segmente zerlegt und segmentweise
/// verglichen; `**` konsumiert null oder mehr ganze Segmente. Der Pfad
/// stammt vom Modell: der Abgleich läuft deshalb iterativ (siehe
/// [`wildcard_match_by`]), ohne Rekursion pro Zeichen oder Segment.
fn glob_match_path(pattern: &str, path: &str) -> bool {
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    wildcard_match_by(
        &pattern_segments,
        &path_segments,
        |segment_pattern| *segment_pattern == "**",
        |segment_pattern, segment| segment_match(segment_pattern, segment),
    )
}

/// Vergleicht ein einzelnes Pfadsegment gegen ein Muster mit `*` (null oder
/// mehr Zeichen) und `?` (genau ein Zeichen).
fn segment_match(pattern: &str, segment: &str) -> bool {
    wildcard_match_by(
        pattern.as_bytes(),
        segment.as_bytes(),
        |&byte| byte == b'*',
        |&expected, &actual| expected == b'?' || expected == actual,
    )
}

/// Iterativer Wildcard-Abgleich mit Rücksprung zum zuletzt gesehenen Stern:
/// O(Muster·Text) Schritte, konstante Stacktiefe. `is_star` markiert ein
/// Musterelement, das null oder mehr Textelemente konsumiert; `matches_one`
/// vergleicht jedes andere Musterelement mit genau einem Textelement.
fn wildcard_match_by<P, T>(
    pattern: &[P],
    text: &[T],
    is_star: impl Fn(&P) -> bool,
    matches_one: impl Fn(&P, &T) -> bool,
) -> bool {
    let (mut p, mut t) = (0, 0);
    // Musterposition hinter dem letzten Stern und die Textposition, ab der
    // dieser Stern beim nächsten Rücksprung ein Element mehr konsumiert.
    let mut backtrack: Option<(usize, usize)> = None;
    while let Some(actual) = text.get(t) {
        match pattern.get(p) {
            Some(expected) if is_star(expected) => {
                p += 1;
                backtrack = Some((p, t));
            }
            Some(expected) if matches_one(expected, actual) => {
                p += 1;
                t += 1;
            }
            _ => match backtrack {
                Some((star_p, star_t)) => {
                    p = star_p;
                    t = star_t + 1;
                    backtrack = Some((star_p, t));
                }
                None => return false,
            },
        }
    }
    pattern.iter().skip(p).all(is_star)
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
    use crate::test_support::{TestError, TestResult, ctx};
    use serde_json::json;

    fn rule(
        tool: &str,
        pattern: Option<&str>,
        decision: RuleDecision,
        scope: RuleScope,
    ) -> ApprovalRule {
        ApprovalRule {
            tool: tool.to_owned(),
            pattern: pattern.map(str::to_owned),
            decision,
            scope,
        }
    }

    /// Plan R9, Teil F: `job.start` wird wie `shell.exec` ausgewertet —
    /// über `command` oder das gequotete `argv`; `env` verhindert Allow.
    #[test]
    fn test_evaluate_job_start_like_shell_exec() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "job.start",
            Some("cargo build"),
            RuleDecision::Allow,
            RuleScope::Session,
        ));
        assert_eq!(
            rules.evaluate("job.start", &json!({"command": "cargo build --release"})),
            Some(RuleDecision::Allow)
        );
        assert_eq!(
            rules.evaluate("job.start", &json!({"argv": ["cargo", "build", "-p", "x"]})),
            Some(RuleDecision::Allow)
        );
        assert_eq!(
            rules.evaluate("job.start", &json!({"command": "cargo build; rm -rf /"})),
            None
        );
        assert_eq!(
            rules.evaluate(
                "job.start",
                &json!({"command": "cargo build", "env": ["RUSTC_WRAPPER=/tmp/x"]})
            ),
            None
        );
        assert_eq!(rules.evaluate("job.start", &json!({"name": "x"})), None);
        // Eine `shell.exec`-Regel gilt nicht für `job.start`.
        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "cargo build"})),
            None
        );
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

        assert_eq!(
            result, None,
            "a dangerous compound command must never be allowed by a prefix rule"
        );
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

    /// Runde 5, Teil E: `match = "cargo test*"` — angehängtes `*` am letzten
    /// Token ist ein Präfix-Glob nur für dieses Token.
    #[test]
    fn test_evaluate_shell_trailing_glob_on_last_token() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "shell.exec",
            Some("cargo test*"),
            RuleDecision::Allow,
            RuleScope::Session,
        ));

        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "cargo test --workspace"})),
            Some(RuleDecision::Allow)
        );
        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "cargo tests"})),
            Some(RuleDecision::Allow)
        );
        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "cargo build"})),
            None
        );
        assert_eq!(
            rules.evaluate("shell.exec", &json!({"command": "cargo test; rm -rf /"})),
            None,
            "ein zusammengesetzter Befehl trifft nie eine Allow-Regel"
        );
    }

    /// Ein einzelnes `&` ist in sh auch mitten im Wort ein Steueroperator:
    /// `git status &rm -rf ~` darf die Regel `git status` nicht treffen —
    /// weder für `shell.exec` noch für `job.start`.
    #[test]
    fn test_evaluate_bare_ampersand_never_allowed() {
        let rules = AllowRuleSet::new();
        for tool in ["shell.exec", "job.start"] {
            rules.add(rule(
                tool,
                Some("git status"),
                RuleDecision::Allow,
                RuleScope::Session,
            ));
            rules.add(rule(
                tool,
                Some("cargo test*"),
                RuleDecision::Allow,
                RuleScope::Session,
            ));
        }

        for tool in ["shell.exec", "job.start"] {
            for command in [
                "git status &rm -rf ~",
                "git status&touch x",
                "git status --short &curl -o f http://x",
                "git status\rrm -rf ~",
                "cargo test&x",
            ] {
                assert_eq!(
                    rules.evaluate(tool, &json!({"command": command})),
                    None,
                    "{tool}: {command:?} darf keine Allow-Regel treffen"
                );
            }
            assert_eq!(
                rules.evaluate(tool, &json!({"command": "git status --short"})),
                Some(RuleDecision::Allow)
            );
        }
        assert_eq!(derive_shell_rule("git status&touch x"), None);
    }

    /// Eine Deny-Regel prüft jedes Teilstück eines zusammengesetzten
    /// Befehls, nicht nur das erste — sonst hebelt ein harmloser Anfang sie
    /// unter FullAccess aus.
    #[test]
    fn test_evaluate_deny_matches_any_segment() {
        let rules = AllowRuleSet::new();
        for tool in ["shell.exec", "job.start"] {
            rules.add(rule(tool, Some("rm"), RuleDecision::Deny, RuleScope::Session));
        }

        for tool in ["shell.exec", "job.start"] {
            for command in [
                "true; rm -rf ~",
                "echo a|rm x",
                "git status &rm -rf ~",
                "echo $(rm x)",
                "(rm x)",
                "true\nrm x",
            ] {
                assert_eq!(
                    rules.evaluate(tool, &json!({"command": command})),
                    Some(RuleDecision::Deny),
                    "{tool}: {command:?} muss die Deny-Regel treffen"
                );
            }
            for command in ["rmdir x", "echo rm; ls"] {
                assert_eq!(
                    rules.evaluate(tool, &json!({"command": command})),
                    None,
                    "{tool}: {command:?} trifft `rm` nur als ganzes erstes Token"
                );
            }
        }
    }

    /// Ein vom Modell geliefertes, sehr langes Pfadsegment darf den Abgleich
    /// nicht in eine Rekursion pro Byte treiben, viele Segmente mit mehreren
    /// `**` nicht in polynomielle Laufzeit: ein Thread mit 256 KiB Stack muss
    /// beides zügig überstehen.
    #[test]
    fn test_glob_long_path_runs_iteratively() -> TestResult {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "fs.*",
            Some("src/*"),
            RuleDecision::Allow,
            RuleScope::Project,
        ));

        let worker = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let long_segment = format!("src/{}", "a".repeat(1_000_000));
                let many_segments = format!("{}q", "a/".repeat(100_000));
                (
                    glob_match_path("src/*", &long_segment),
                    rules.evaluate("fs.read", &json!({"path": long_segment})),
                    glob_match_path("**/a/**/a/**/b", &many_segments),
                )
            })
            .map_err(ctx("Thread mit kleinem Stack starten"))?;
        let (long_match, long_decision, many_match) = worker.join().map_err(|_| {
            TestError::Unexpected("Glob-Abgleich im kleinen Stack abgebrochen".to_owned())
        })?;

        assert!(long_match);
        assert_eq!(long_decision, Some(RuleDecision::Allow));
        assert!(!many_match);
        Ok(())
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
        assert!(glob_match_path(
            "/home/user/**/*.rs",
            "/home/user/a/b/c/main.rs"
        ));
        assert!(glob_match_path("/home/user/**/*.rs", "/home/user/main.rs"));
        assert!(!glob_match_path(
            "/home/user/**/*.rs",
            "/home/user/main.txt"
        ));
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
        rules.add(rule(
            "shell.exec",
            None,
            RuleDecision::Allow,
            RuleScope::Session,
        ));

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
        let a = rule(
            "shell.exec",
            Some("git status"),
            RuleDecision::Allow,
            RuleScope::Session,
        );
        let b = rule(
            "shell.exec",
            Some("git status"),
            RuleDecision::Allow,
            RuleScope::Session,
        );

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
        let seed = vec![rule(
            "shell.exec",
            None,
            RuleDecision::Allow,
            RuleScope::Global,
        )];
        let rules = AllowRuleSet::from_rules(seed.clone());
        assert_eq!(rules.snapshot(), seed);
    }

    #[test]
    fn test_other_tool_only_matches_pattern_none_rules() {
        let rules = AllowRuleSet::new();
        rules.add(rule(
            "agent.spawn",
            Some("anything"),
            RuleDecision::Allow,
            RuleScope::Session,
        ));
        rules.add(rule(
            "agent.stop",
            None,
            RuleDecision::Allow,
            RuleScope::Session,
        ));

        assert_eq!(rules.evaluate("agent.spawn", &json!({})), None);
        assert_eq!(
            rules.evaluate("agent.stop", &json!({})),
            Some(RuleDecision::Allow)
        );
    }

    #[test]
    fn test_derive_shell_rule_two_tokens_when_second_is_not_a_flag() {
        assert_eq!(
            derive_shell_rule("git status --short"),
            Some("git status".to_owned())
        );
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
