//! Lernen aus Freigaben (Runde 5, Teil E3).
//!
//! # Verantwortlichkeit
//! Zählt manuell freigegebene, **gleichartige** Werkzeugaufrufe einer
//! Sitzung und bietet ab der dritten gleichartigen Freigabe an, daraus eine
//! Allow-Regel zu machen („Ja, und künftig erlauben: `<muster>`"). Geschrieben
//! wird hier nichts — die Oberfläche legt die Regel erst nach Bestätigung an
//! (Sitzung: geteilte `AllowRuleSet`; Projekt: zusätzlich über
//! `/permissions allow … --project`, also den `ConfigWriter`).
//!
//! # Gleichartigkeit ([`learn_key_for`])
//! - `shell.exec`: Werkzeug plus konservativ abgeleitetes Befehlspräfix
//!   ([`harw_extension_api::allow_rules::derive_shell_rule`], z. B.
//!   `cargo test` → Muster `cargo test *`). Zusammengesetzte Befehle und
//!   breite Interpreter (`bash`, `python`, `sudo` …) ergeben keinen Schlüssel.
//! - schreibende `fs.*`-Werkzeuge: Werkzeug plus Verzeichnis-Glob des
//!   Zielpfads (`src/lib/a.rs` → `src/lib/**`); Pfade mit `..` ergeben keinen
//!   Schlüssel.
//! - andere Werkzeuge: nur der Werkzeugname (Regel ohne Muster).
//!
//! # Nie gelernt
//! - `ALWAYS_ASK_TOOLS` (`process.kill`, `host.sudo_exec`, `agents.*commit*`,
//!   `skills.commit_proposal`, `agents.write_uia`);
//! - Aufrufe, die der Vorfilter des Auto-Modus als riskant einstuft (der
//!   Aufrufer übergibt das als `risky`);
//! - `shell.exec`/`fs.*` ohne ableitbares Muster.

use std::collections::HashMap;

use harw_extension_api::ToolCall;
use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope, derive_shell_rule};
use harw_registry_defaults::ALWAYS_ASK_TOOLS;

/// Ab der wievielten gleichartigen Freigabe das Angebot erscheint.
pub const LEARN_OFFER_THRESHOLD: u32 = 3;

/// Nur-lesende `fs.*`-Werkzeuge (brauchen keine Regel, sind ohnehin frei).
const READ_ONLY_FS_TOOLS: &[&str] = &["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep"];

/// Schlüssel einer Gruppe gleichartiger Aufrufe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LearnKey {
    /// Werkzeugname.
    pub tool: String,
    /// Muster für [`ApprovalRule::pattern`]; `None` = jeder Aufruf.
    pub pattern: Option<String>,
}

impl LearnKey {
    /// Anzeigeform, z. B. `shell.exec cargo test *` oder `fs.write src/**`.
    #[must_use]
    pub fn display(&self) -> String {
        match (&self.pattern, self.tool.as_str()) {
            (Some(pattern), "shell.exec") => format!("{} {pattern} *", self.tool),
            (Some(pattern), _) => format!("{} {pattern}", self.tool),
            (None, _) => format!("{} (alle Aufrufe)", self.tool),
        }
    }
}

/// Das Angebot „künftig erlauben".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnOffer {
    /// Die Gruppe.
    pub key: LearnKey,
    /// Bisherige gleichartige Freigaben (ohne die jetzt anstehende).
    pub approvals: u32,
}

impl LearnOffer {
    /// Die Allow-Regel, die bei Annahme angelegt wird.
    ///
    /// # Arguments
    /// - `scope` ([`RuleScope`]): `Session` oder `Project`.
    #[must_use]
    pub fn rule(&self, scope: RuleScope) -> ApprovalRule {
        ApprovalRule {
            tool: self.key.tool.clone(),
            pattern: self.key.pattern.clone(),
            decision: RuleDecision::Allow,
            scope,
        }
    }
}

/// Leitet den Gleichartigkeits-Schlüssel eines Aufrufs ab.
///
/// # Rückgabe
/// `None` für `ALWAYS_ASK_TOOLS`, lesende `fs.*`-Werkzeuge und Aufrufe ohne
/// sicher ableitbares Muster (siehe Modul-Doku).
#[must_use]
pub fn learn_key_for(call: &ToolCall) -> Option<LearnKey> {
    let tool = call.name.as_str();
    if ALWAYS_ASK_TOOLS.contains(&tool) || READ_ONLY_FS_TOOLS.contains(&tool) {
        return None;
    }
    let pattern = if tool == "shell.exec" {
        let command = call.arguments.get("command").and_then(|v| v.as_str())?;
        Some(derive_shell_rule(command)?)
    } else if tool.starts_with("fs.") {
        let path = call.arguments.get("path").and_then(|v| v.as_str())?;
        Some(directory_glob(path)?)
    } else {
        None
    };
    Some(LearnKey {
        tool: tool.to_owned(),
        pattern,
    })
}

/// `src/lib/a.rs` → `src/lib/**`; eine Datei im Wurzelverzeichnis bleibt
/// exakt. `None` bei leerem Pfad oder `..`.
fn directory_glob(path: &str) -> Option<String> {
    let path = path.trim();
    if path.is_empty() || path.split('/').any(|segment| segment == "..") {
        return None;
    }
    match path.rsplit_once('/') {
        Some(("", _)) => None,
        Some((dir, _)) => Some(format!("{dir}/**")),
        None => Some(path.to_owned()),
    }
}

/// Zählt manuelle Freigaben je [`LearnKey`] (sitzungslokal).
#[derive(Debug, Default)]
pub struct ApprovalLearner {
    counts: HashMap<LearnKey, u32>,
}

impl ApprovalLearner {
    /// Hält eine manuelle Freigabe fest.
    ///
    /// # Arguments
    /// - `call` (`&ToolCall`): der freigegebene Aufruf.
    /// - `risky` (`bool`): ob der Vorfilter ihn als riskant einstuft — dann
    ///   wird nicht gezählt.
    ///
    /// # Rückgabe
    /// Der neue Zählerstand, oder `None`, wenn der Aufruf nie gelernt wird.
    pub fn record_approval(&mut self, call: &ToolCall, risky: bool) -> Option<u32> {
        if risky {
            return None;
        }
        let key = learn_key_for(call)?;
        let count = self.counts.entry(key).or_insert(0);
        *count = count.saturating_add(1);
        Some(*count)
    }

    /// Das Angebot für einen gerade erfragten Aufruf.
    ///
    /// # Beschreibung
    /// Erscheint, wenn dieser Aufruf die [`LEARN_OFFER_THRESHOLD`]-te (oder
    /// eine spätere) gleichartige Freigabe wäre — also bei mindestens zwei
    /// bisherigen — und er weder riskant noch unlernbar ist.
    #[must_use]
    pub fn offer_for(&self, call: &ToolCall, risky: bool) -> Option<LearnOffer> {
        if risky {
            return None;
        }
        let key = learn_key_for(call)?;
        let approvals = self.counts.get(&key).copied().unwrap_or(0);
        (approvals.saturating_add(1) >= LEARN_OFFER_THRESHOLD)
            .then_some(LearnOffer { key, approvals })
    }

    /// Setzt den Zähler einer Gruppe zurück (Regel angelegt).
    pub fn forget(&mut self, key: &LearnKey) {
        self.counts.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::ToolName;
    use harw_types::ToolCallId;
    use serde_json::json;

    fn call(tool: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(tool),
            arguments,
        }
    }

    fn shell(command: &str) -> ToolCall {
        call("shell.exec", json!({ "command": command }))
    }

    /// Das Angebot erscheint erst beim dritten gleichartigen Aufruf.
    #[test]
    fn the_offer_appears_only_at_the_third_similar_approval() {
        let mut learner = ApprovalLearner::default();
        assert_eq!(learner.offer_for(&shell("cargo test -p a"), false), None);
        learner.record_approval(&shell("cargo test -p a"), false);
        assert_eq!(learner.offer_for(&shell("cargo test -p b"), false), None);
        learner.record_approval(&shell("cargo test --workspace"), false);

        let offer = learner.offer_for(&shell("cargo test"), false);
        assert_eq!(
            offer.as_ref().map(|offer| offer.key.display()),
            Some("shell.exec cargo test *".to_owned())
        );
        assert_eq!(offer.map(|offer| offer.approvals), Some(2));
        assert_eq!(
            learner.offer_for(&shell("cargo build"), false),
            None,
            "andere Gruppe zählt getrennt"
        );
    }

    /// Nie für riskante Muster (Vorfilter-Treffer) und nie für breite
    /// Interpreter oder zusammengesetzte Befehle.
    #[test]
    fn the_offer_never_appears_for_risky_patterns() {
        let mut learner = ApprovalLearner::default();
        for _ in 0..5 {
            assert_eq!(
                learner.record_approval(&shell("git push --force"), true),
                None
            );
            assert_eq!(learner.record_approval(&shell("bash -c make"), false), None);
            assert_eq!(
                learner.record_approval(&shell("make && rm -rf /"), false),
                None
            );
        }
        assert_eq!(learner.offer_for(&shell("git push --force"), true), None);
        assert_eq!(learner.offer_for(&shell("bash -c make"), false), None);
        assert_eq!(learner.offer_for(&shell("make && rm -rf /"), false), None);
    }

    /// Nie für `ALWAYS_ASK_TOOLS`.
    #[test]
    fn the_offer_never_appears_for_always_ask_tools() {
        let mut learner = ApprovalLearner::default();
        for tool in ALWAYS_ASK_TOOLS {
            for _ in 0..5 {
                assert_eq!(learner.record_approval(&call(tool, json!({})), false), None);
            }
            assert_eq!(
                learner.offer_for(&call(tool, json!({})), false),
                None,
                "{tool}"
            );
        }
    }

    #[test]
    fn fs_writes_learn_a_directory_glob() {
        let mut learner = ApprovalLearner::default();
        learner.record_approval(&call("fs.write", json!({"path": "src/lib/a.rs"})), false);
        learner.record_approval(&call("fs.edit", json!({"path": "src/lib/b.rs"})), false);
        learner.record_approval(&call("fs.write", json!({"path": "src/lib/c.rs"})), false);
        let offer = learner.offer_for(&call("fs.write", json!({"path": "src/lib/d.rs"})), false);
        assert_eq!(
            offer.map(|offer| offer.rule(RuleScope::Session)),
            Some(ApprovalRule {
                tool: "fs.write".to_owned(),
                pattern: Some("src/lib/**".to_owned()),
                decision: RuleDecision::Allow,
                scope: RuleScope::Session,
            })
        );
        assert_eq!(
            learn_key_for(&call("fs.write", json!({"path": "../x/y.rs"}))),
            None
        );
    }

    #[test]
    fn forget_resets_the_group() {
        let mut learner = ApprovalLearner::default();
        for _ in 0..3 {
            learner.record_approval(&shell("cargo check"), false);
        }
        let offer = learner.offer_for(&shell("cargo check"), false);
        assert!(offer.is_some());
        if let Some(offer) = offer {
            learner.forget(&offer.key);
        }
        assert_eq!(learner.offer_for(&shell("cargo check"), false), None);
    }
}
