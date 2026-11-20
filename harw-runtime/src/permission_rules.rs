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
//!
//! # DoD-Taint (PL-100 Phase 4, B22)
//! Eine gelernte Allow-Regel (etwa für `job.stop`) oder ein `allow` des
//! Auto-Modus-Klassifizierers hebt die Taint-Regel nie auf: nach einer
//! DoD-Abfrage im selben Turn liefert die
//! `harw_core::policy::DodTaintPolicy` `AskUser`, und `check_approval`
//! aggregiert `Deny` > `AskUser` > `Allow`. Die Tests unten halten außerdem
//! die Werkzeuglisten von `harw_core::dod_taint` mit `harw-tool-job` und
//! `harw-registry-defaults` sowie `harw_core::policy::NON_INTERACTIVE_ASK_LABELS`
//! mit `crate::approval::AskResolutionPolicy::label_for` im Gleichlauf.

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
            (Some(pattern), "shell.exec" | harw_tool_job::JOB_START_TOOL) => {
                format!("{} {pattern} *", self.tool)
            }
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
    } else if tool == harw_tool_job::JOB_START_TOOL {
        // Plan R9, Teil F: `job.start` wie `shell.exec` — Muster aus
        // `command` bzw. dem gequotetem `argv`. Mit gesetztem `env` gibt es
        // kein Angebot (eine Allow-Regel träfe solche Aufrufe ohnehin nie).
        let has_env = call
            .arguments
            .get("env")
            .and_then(|v| v.as_array())
            .is_some_and(|env| !env.is_empty());
        if has_env {
            return None;
        }
        let command = harw_tool_job::job_start_command_text(&call.arguments)?;
        Some(derive_shell_rule(&command)?)
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

    /// Plan R9, Teil F: `job.start` lernt wie `shell.exec` — aus `command`
    /// oder dem gequoteten `argv`; mit `env` gibt es kein Angebot.
    #[test]
    fn job_start_is_learned_like_shell_exec() {
        let by_command = learn_key_for(&call(
            "job.start",
            json!({"command": "cargo test -p a", "name": "t"}),
        ));
        let by_argv = learn_key_for(&call(
            "job.start",
            json!({"argv": ["cargo", "test", "--workspace"], "name": "t"}),
        ));
        let shell_key = learn_key_for(&shell("cargo test -p a"));
        assert_eq!(
            by_command.as_ref().map(|key| key.pattern.clone()),
            shell_key.as_ref().map(|key| key.pattern.clone())
        );
        assert_eq!(
            by_command.as_ref().map(LearnKey::display),
            Some("job.start cargo test *".to_owned())
        );
        assert_eq!(by_argv, by_command);
        assert_eq!(
            learn_key_for(&call(
                "job.start",
                json!({"command": "cargo test", "env": ["RUSTFLAGS=-Cx"], "name": "t"})
            )),
            None
        );
        // Die angebotene Regel trifft danach tatsächlich `job.start`.
        let rules = harw_extension_api::allow_rules::AllowRuleSet::new();
        if let Some(key) = by_command {
            rules.add(LearnOffer { key, approvals: 3 }.rule(RuleScope::Session));
        }
        assert_eq!(
            rules.evaluate(
                "job.start",
                &json!({"command": "cargo test -p z", "name": "t"})
            ),
            Some(RuleDecision::Allow)
        );
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

    // ------------------------------------------------------------------
    // PL-100 Phase 4 (B22, O1): DoD-Taint gegen gelernte Regeln
    // ------------------------------------------------------------------

    use crate::test_support::{TestError, TestResult};
    use harw_core::dod_taint::{
        JOB_READ_ONLY_TOOLS, TAINT_GATED_HOST_TOOLS, is_dod_tool, is_taint_gated, with_review_taint,
    };
    use harw_core::policy::{DodTaintPolicy, NON_INTERACTIVE_ASK_LABELS};
    use harw_extension_api::allow_rules::AllowRuleSet;
    use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
    use harw_extension_api::auto_mode::{
        AutoApprovalGate, AutoDecision, AutoVerdict, VerdictSource,
    };
    use harw_extension_api::{ApprovalDecision, ApprovalHandler, ExtFuture};
    use harw_registry_defaults::{AUTO_APPROVED_TOOLS, DefaultApprovalPolicy};

    /// Die Liste lesender Job-Werkzeuge in `harw_core::dod_taint` ist genau
    /// `harw_tool_job::JOB_READ_TOOLS`; jedes andere Job-Werkzeug ist
    /// taint-pflichtig.
    #[test]
    fn dod_taint_job_lists_match_the_job_crate() -> TestResult {
        let mut core_read: Vec<&str> = JOB_READ_ONLY_TOOLS.to_vec();
        let mut job_read: Vec<&str> = harw_tool_job::JOB_READ_TOOLS.to_vec();
        core_read.sort_unstable();
        job_read.sort_unstable();
        assert_eq!(core_read, job_read);
        for tool in harw_tool_job::JOB_TOOL_NAMES {
            let read_only = harw_tool_job::JOB_READ_TOOLS.contains(&tool);
            if read_only == is_taint_gated(tool) {
                return Err(TestError::Unexpected(format!(
                    "{tool}: lesend={read_only}, aber taint-pflichtig={}",
                    is_taint_gated(tool)
                )));
            }
        }
        Ok(())
    }

    /// Die Host-Werkzeuge der Taint-Regel fragen unter `ask`/`auto` ohnehin
    /// immer (und werden nie gelernt); die lesenden Job- und DoD-Werkzeuge
    /// sind auto-freigegeben und nie taint-pflichtig.
    #[test]
    fn dod_taint_lists_agree_with_the_default_policy() -> TestResult {
        for tool in TAINT_GATED_HOST_TOOLS {
            assert!(ALWAYS_ASK_TOOLS.contains(tool), "{tool}");
            assert_eq!(learn_key_for(&call(tool, json!({}))), None, "{tool}");
        }
        for tool in JOB_READ_ONLY_TOOLS {
            assert!(AUTO_APPROVED_TOOLS.contains(tool), "{tool}");
        }
        let dod_tools: Vec<&str> = AUTO_APPROVED_TOOLS
            .iter()
            .copied()
            .filter(|tool| is_dod_tool(tool))
            .collect();
        if dod_tools.is_empty() {
            return Err(TestError::Missing("auto-freigegebene dod.*-Werkzeuge"));
        }
        for tool in dod_tools {
            assert!(!is_taint_gated(tool), "{tool}");
        }
        Ok(())
    }

    /// Ein **Modell** von `harw_core::turn_loop::check_approval`, nicht die
    /// echte Funktion: beide Handler im selben Prüfkontext, Aggregation
    /// `Deny` > `AskUser` > `Allow` von Hand nachgebaut.
    ///
    /// # Beschreibung
    /// Die Tests darauf zeigen nur, dass sich `DefaultApprovalPolicy` und
    /// `DodTaintPolicy` grundsätzlich so kombinieren lassen. Ob das echte
    /// `check_approval` den Prüfkontext setzt und so aggregiert, prüfen die
    /// Durchläufe in `dod_taint_turns` (echter `run_turn` über der echten
    /// `crate::approval::ApprovalChain`).
    async fn aggregate(
        default: &DefaultApprovalPolicy,
        taint: &DodTaintPolicy,
        call: &ToolCall,
        tainted: bool,
    ) -> ApprovalDecision {
        with_review_taint(tainted, async {
            let decisions = [default.review(call).await, taint.review(call).await];
            let deny = decisions
                .iter()
                .find(|decision| matches!(decision, ApprovalDecision::Deny(_)))
                .cloned();
            let ask = decisions
                .iter()
                .find(|decision| matches!(decision, ApprovalDecision::AskUser(_)))
                .cloned();
            deny.or(ask).unwrap_or(ApprovalDecision::Allow)
        })
        .await
    }

    /// Auto-Modus mit einer gelernten Allow-Regel für `job.stop`: ohne
    /// DoD-Abfrage gibt die Regel frei, nach einer DoD-Abfrage im selben Turn
    /// fragt der Taint trotzdem; unter Full Access fragt nichts (O1).
    #[tokio::test]
    async fn dod_taint_beats_a_learned_allow_rule_but_not_full_access() -> TestResult {
        let rules = harw_extension_api::allow_rules::AllowRuleSet::new();
        let stop = call("job.stop", json!({"id": "job-1"}));
        let key = learn_key_for(&stop).ok_or(TestError::Missing("Lernschlüssel job.stop"))?;
        rules.add(LearnOffer { key, approvals: 3 }.rule(RuleScope::Session));
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let default = DefaultApprovalPolicy::with_rules(cell.clone(), rules);
        let taint = DodTaintPolicy::new().with_mode(cell.clone());

        assert!(matches!(
            aggregate(&default, &taint, &stop, false).await,
            ApprovalDecision::Allow
        ));
        assert!(matches!(
            aggregate(&default, &taint, &stop, true).await,
            ApprovalDecision::AskUser(_)
        ));

        cell.set(ApprovalMode::FullAccess);
        assert!(matches!(
            aggregate(&default, &taint, &stop, true).await,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// `process.kill` und `host.sudo_exec` fragen unter `ask`/`auto` in
    /// jedem Fall — mit und ohne Taint, auch gegen eine Allow-Regel.
    #[tokio::test]
    async fn host_tools_ask_with_and_without_taint() -> TestResult {
        for tool in TAINT_GATED_HOST_TOOLS {
            let rules = harw_extension_api::allow_rules::AllowRuleSet::new();
            rules.add(ApprovalRule {
                tool: (*tool).to_owned(),
                pattern: None,
                decision: RuleDecision::Allow,
                scope: RuleScope::Session,
            });
            for mode in [ApprovalMode::AlwaysAsk, ApprovalMode::Delegated] {
                let cell = ApprovalModeCell::new(mode);
                let default = DefaultApprovalPolicy::with_rules(cell.clone(), rules.clone());
                let taint = DodTaintPolicy::new().with_mode(cell);
                let host = call(tool, json!({}));
                for tainted in [false, true] {
                    assert!(
                        matches!(
                            aggregate(&default, &taint, &host, tainted).await,
                            ApprovalDecision::AskUser(_)
                        ),
                        "{tool} {mode:?} tainted={tainted}"
                    );
                }
            }
        }
        Ok(())
    }

    /// Ein Auto-Modus-Gate, das jeden Aufruf freigibt — steht für einen
    /// Klassifizierer, der `allow` antwortet.
    #[derive(Debug)]
    struct AllowGate;

    impl AutoApprovalGate for AllowGate {
        fn decide<'a>(&'a self, _call: &'a ToolCall) -> ExtFuture<'a, AutoVerdict> {
            Box::pin(async {
                AutoVerdict::new(
                    AutoDecision::Allow,
                    "test",
                    "Stub-Klassifizierer gibt frei",
                    VerdictSource::Classifier,
                )
            })
        }
    }

    /// Echter Auto-Modus (Standardpolitik mit Klassifizierer-Gate): ohne
    /// DoD-Abfrage gibt der Klassifizierer `job.stop` frei, nach einer
    /// DoD-Abfrage im selben Turn fragt der Taint trotzdem; unter Full Access
    /// fragt nichts (O1).
    #[tokio::test]
    async fn dod_taint_beats_the_auto_mode_classifier_but_not_full_access() -> TestResult {
        let cell = ApprovalModeCell::new(ApprovalMode::Delegated);
        let stop = call("job.stop", json!({"id": "job-1"}));

        // Ohne Gate fragt die Standardpolitik selbst: die Freigabe unten
        // kommt also wirklich vom Klassifizierer-Pfad.
        let without_gate = DefaultApprovalPolicy::with_rules(cell.clone(), AllowRuleSet::new());
        assert!(matches!(
            without_gate.review(&stop).await,
            ApprovalDecision::AskUser(_)
        ));

        let default = DefaultApprovalPolicy::with_rules(cell.clone(), AllowRuleSet::new())
            .with_auto_gate(std::sync::Arc::new(AllowGate));
        let taint = DodTaintPolicy::new().with_mode(cell.clone());

        // Steht `job.stop` einmal in `ALWAYS_ASK_TOOLS` (Phase 3.5), fragt es
        // schon ohne Taint; der Klassifizierer entscheidet dann nie.
        let without_taint = aggregate(&default, &taint, &stop, false).await;
        if ALWAYS_ASK_TOOLS.contains(&"job.stop") {
            assert!(matches!(without_taint, ApprovalDecision::AskUser(_)));
        } else {
            assert!(matches!(without_taint, ApprovalDecision::Allow));
        }
        assert!(matches!(
            aggregate(&default, &taint, &stop, true).await,
            ApprovalDecision::AskUser(_)
        ));

        cell.set(ApprovalMode::FullAccess);
        assert!(matches!(
            aggregate(&default, &taint, &stop, true).await,
            ApprovalDecision::Allow
        ));
        Ok(())
    }

    /// `harw_core::policy::NON_INTERACTIVE_ASK_LABELS` ist genau die Menge
    /// der Labels, die `AskResolutionPolicy::label_for` für nicht
    /// interaktive Einstiege vergibt.
    #[test]
    fn non_interactive_ask_labels_match_the_runtime_ask_resolution() -> TestResult {
        use crate::approval::AskResolutionPolicy;
        use crate::spec::AskResolution;

        for resolution in [
            AskResolution::Interactive,
            AskResolution::RejectTurn,
            AskResolution::BlockJob,
            AskResolution::Fail,
        ] {
            // Ohne Platzhalter: eine neue Variante muss hier eingeordnet
            // werden.
            let non_interactive = match resolution {
                AskResolution::Interactive => false,
                AskResolution::RejectTurn | AskResolution::BlockJob | AskResolution::Fail => true,
            };
            let label = AskResolutionPolicy::label_for(resolution);
            assert_eq!(
                NON_INTERACTIVE_ASK_LABELS.contains(&label),
                non_interactive,
                "{label}"
            );
        }
        assert_eq!(NON_INTERACTIVE_ASK_LABELS.len(), 3);
        Ok(())
    }

    /// Die echte Kette eines nicht interaktiven Einstiegs gilt als „niemand
    /// kann antworten"; die eines interaktiven nicht.
    #[test]
    fn real_chains_tell_whether_a_taint_question_is_answerable() -> TestResult {
        use crate::approval::ApprovalChain;
        use crate::spec::AskResolution;

        for (resolution, answerable) in [
            (AskResolution::Interactive, true),
            (AskResolution::RejectTurn, false),
            (AskResolution::BlockJob, false),
            (AskResolution::Fail, false),
        ] {
            let chain = ApprovalChain::for_root(
                &harw_config::ResolvedConfig::default(),
                resolution,
                ApprovalModeCell::new(ApprovalMode::Delegated),
                None,
                AllowRuleSet::new(),
            );
            assert_eq!(
                DodTaintPolicy::ask_answerable(&chain.handlers(), true),
                answerable,
                "{resolution:?}"
            );
            assert_eq!(
                DodTaintPolicy::ask_answerable(&chain.for_child().handlers(), true),
                answerable,
                "Kind {resolution:?}"
            );
        }
        Ok(())
    }

    /// PL-100 Phase 4 (B22, O1) Ende zu Ende über die **echte** Freigabekette.
    ///
    /// # Beschreibung
    /// Anders als `aggregate` (ein Modell von `check_approval`) laufen
    /// diese Tests durch den echten `harw_core::run_turn`: das echte
    /// `check_approval` befragt genau die Handler, die
    /// `crate::approval::ApprovalChain::install` montiert —
    /// Standardpolitik mit Regeln, Ask-Auflösung und die DoD-Taint-Politik
    /// der Kette selbst (nie von Hand dazugesteckt).
    ///
    /// Zwei Tests (`every_production_chain_installs_a_mode_aware_dod_taint_policy`,
    /// `full_access_never_asks_or_denies_after_a_dod_query`) schlagen fehl,
    /// solange `ApprovalChain` die `DodTaintPolicy` nicht montiert: dann
    /// liefe jede Sitzung über den modusblinden Fallback des Turn-Loops, der
    /// unter Full Access nachfragt bzw. ablehnt (Verstoß gegen O1).
    mod dod_taint_turns {
        use std::collections::VecDeque;
        use std::sync::{Arc, Mutex, PoisonError};

        use harw_core::dod_taint::{TAINT_REASON, pending_ask_reason, with_review_taint};
        use harw_core::model::{
            ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse,
        };
        use harw_core::policy::DOD_TAINT_POLICY_LABEL;
        use harw_core::{AgentSession, InMemoryStateStore, SpawnContext, TurnInput, TurnOutcome};
        use harw_extension_api::allow_rules::{
            AllowRuleSet, ApprovalRule, RuleDecision, RuleScope,
        };
        use harw_extension_api::approval_mode::{ApprovalMode, ApprovalModeCell};
        use harw_extension_api::contributors::ApprovalHandlerKind;
        use harw_extension_api::{
            ApprovalDecision, ApprovalHandler, ExtensionRegistryBuilder, ToolProvider,
        };
        use harw_protocol::items::{ToolCallResult, TurnItem};
        use harw_tools::{
            FunctionToolSpec, JsonSchema, ToolCall, ToolExecutionContext, ToolExecutor,
            ToolExecutorFuture, ToolName, ToolOutput, ToolSpec,
        };
        use harw_types::{AgentRole, ApprovalActor, ToolCallId};

        use crate::approval::ApprovalChain;
        use crate::spec::AskResolution;
        use crate::test_support::{TestError, TestResult, ctx};

        /// Die Werkzeuge der Tests: der Auslöser und die Eingriffe.
        const TOOLS: &[&str] = &["dod.findings", "job.stop", "process.kill", "host.sudo_exec"];

        /// Alle Ask-Auflösungen, interaktiv zuerst.
        const RESOLUTIONS: [AskResolution; 4] = [
            AskResolution::Interactive,
            AskResolution::RejectTurn,
            AskResolution::BlockJob,
            AskResolution::Fail,
        ];

        /// Liefert eine vorprogrammierte Folge von Antworten, eine je
        /// Modellaufruf.
        struct ScriptedModel {
            // Die noch offenen Antworten, älteste zuerst.
            responses: Mutex<VecDeque<ModelResponse>>,
        }

        impl ScriptedModel {
            // Ein Modell, das `responses` in dieser Reihenfolge liefert.
            fn new(responses: Vec<ModelResponse>) -> Self {
                Self {
                    responses: Mutex::new(responses.into_iter().collect()),
                }
            }
        }

        impl ModelProvider for ScriptedModel {
            fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
                let next = self
                    .responses
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .pop_front();
                Box::pin(async move { next.ok_or(ModelError::EmptyResponse) })
            }
        }

        /// Bietet [`TOOLS`] an; jeder Aufruf gelingt über [`OkExecutor`].
        struct StubTools;

        impl ToolProvider for StubTools {
            fn tools(&self) -> Vec<ToolSpec> {
                TOOLS
                    .iter()
                    .map(|name| {
                        ToolSpec::Function(FunctionToolSpec {
                            name: ToolName::new(*name),
                            description: format!("stub tool {name}"),
                            parameters: JsonSchema::default(),
                            strict: false,
                        })
                    })
                    .collect()
            }

            fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
                TOOLS
                    .contains(&name.as_str())
                    .then(|| Arc::new(OkExecutor) as Arc<dyn ToolExecutor>)
            }
        }

        /// Gelingt immer mit `ok`.
        struct OkExecutor;

        impl ToolExecutor for OkExecutor {
            fn execute<'a>(
                &'a self,
                _context: &'a ToolExecutionContext,
                _call: &'a ToolCall,
            ) -> ToolExecutorFuture<'a> {
                Box::pin(async { Ok(ToolOutput::text("ok")) })
            }
        }

        /// Spawn-Kontext mit Sandbox auf dem Crate-Verzeichnis und einer
        /// Freigabe-Person: ohne Kontext lehnte der Turn-Loop jede
        /// Ausführung ab, ohne Person jede Rückfrage.
        fn spawn_context() -> TestResult<SpawnContext> {
            let harness_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .ok_or(TestError::Missing(
                    "Workspace-Verzeichnis über harw-runtime",
                ))?
                .to_path_buf();
            let tenant = harw_types::TenantId::from_str("test-tenant");
            let workspace = harw_types::WorkspaceId::from_str("runtime-dod-taint");
            let registry = harw_authority::WorkspaceRegistry::build(
                &harness_root,
                [harw_authority::WorkspaceRegistration {
                    tenant: tenant.clone(),
                    workspace: workspace.clone(),
                    root: std::path::PathBuf::from("harw-runtime"),
                }],
            )
            .map_err(ctx("Test-Workspace ist registrierbar"))?;
            let binding = registry
                .resolve(&tenant, &workspace)
                .map_err(ctx("Test-Workspace löst auf"))?;
            Ok(SpawnContext {
                sandbox: harw_authority::SandboxSpec::from_resolved(
                    binding,
                    harw_authority::PermissionSet::from_policy([
                        harw_authority::Permission::ReadWorkspace,
                    ]),
                ),
                suggestions: None,
                capability_snapshot: None,
                approval_actor: Some(ApprovalActor::Operator {
                    id: "test-operator".to_owned(),
                }),
                organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                allowed_child_orchestrators: Vec::new(),
                trace: None,
                ceiling: None,
            })
        }

        /// Die Wurzelkette eines Einstiegs mit `resolution`, Modus `mode` und
        /// den Regeln `rules` (ohne Responder: eine Rückfrage pausiert den
        /// Turn).
        fn root_chain(
            resolution: AskResolution,
            mode: ApprovalMode,
            rules: AllowRuleSet,
        ) -> ApprovalChain {
            ApprovalChain::for_root(
                &harw_config::ResolvedConfig::default(),
                resolution,
                ApprovalModeCell::new(mode),
                None,
                rules,
            )
        }

        /// Eine Sitzung, deren Registry genau die Handler von `chain` trägt
        /// (montiert über [`ApprovalChain::install`]) und [`StubTools`]
        /// anbietet.
        fn session_on(chain: &ApprovalChain) -> TestResult<AgentSession> {
            let (events, _events_rx) = tokio::sync::mpsc::unbounded_channel();
            let registry = chain
                .install(ExtensionRegistryBuilder::default().tool_provider(Arc::new(StubTools)))
                .build();
            Ok(
                AgentSession::new(AgentRole::Assistant, None, registry, events)
                    .with_spawn_context(spawn_context()?),
            )
        }

        /// Eine gelernte Allow-Regel für jedes `job.stop` (Sitzung).
        fn job_stop_allowed() -> TestResult<AllowRuleSet> {
            let rules = AllowRuleSet::new();
            let added = rules.add(ApprovalRule {
                tool: "job.stop".to_owned(),
                pattern: None,
                decision: RuleDecision::Allow,
                scope: RuleScope::Session,
            });
            if added {
                Ok(rules)
            } else {
                Err(TestError::Unexpected(
                    "die Allow-Regel für job.stop wurde nicht angelegt".to_owned(),
                ))
            }
        }

        /// Ein Aufruf von `name` mit frischer Id.
        fn call(name: &str) -> ToolCall {
            ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new(name),
                arguments: serde_json::json!({"id": "job-1"}),
            }
        }

        /// Eine Modellantwort mit genau den Aufrufen `calls`.
        fn respond(calls: Vec<ToolCall>) -> ModelResponse {
            ModelResponse {
                tool_calls: calls,
                ..ModelResponse::default()
            }
        }

        /// Führt einen Turn mit den Antworten `script` aus.
        async fn run(
            session: &mut AgentSession,
            script: Vec<ModelResponse>,
        ) -> TestResult<TurnOutcome> {
            let store = InMemoryStateStore::new();
            let model = ScriptedModel::new(script);
            harw_core::run_turn(session, &model, &store, TurnInput::user("dod taint"))
                .await
                .map_err(ctx("der Turn läuft"))
        }

        /// Die Werkzeugergebnisse der Sitzung in Verlaufsreihenfolge.
        fn tool_results(session: &AgentSession) -> Vec<ToolCallResult> {
            session
                .history()
                .items()
                .iter()
                .filter_map(|item| match item {
                    TurnItem::ToolResult(result) => Some(result.result.clone()),
                    _ => None,
                })
                .collect()
        }

        /// Jede Produktivkette montiert die `DodTaintPolicy` genau einmal und
        /// über ihrer **eigenen** Modus-Zelle: die Wurzel folgt
        /// `/permissions`, Kind und Enkel folgen dem Modus der Wurzel live.
        /// Damit ist der modusblinde Fallback des Turn-Loops nie der
        /// Produktivpfad.
        #[tokio::test]
        async fn every_production_chain_installs_a_mode_aware_dod_taint_policy() -> TestResult {
            let stop = call("job.stop");
            for resolution in RESOLUTIONS {
                let root = root_chain(resolution, ApprovalMode::Delegated, AllowRuleSet::new());
                let child = root.for_child();
                let grandchild = child.for_child_with_mandate(None);
                for mode in ApprovalMode::ALL {
                    root.mode().set(mode);
                    for (label, chain) in
                        [("Wurzel", &root), ("Kind", &child), ("Enkel", &grandchild)]
                    {
                        let policies: Vec<Arc<dyn ApprovalHandler>> = chain
                            .handlers()
                            .into_iter()
                            .filter(|handler| handler.label() == DOD_TAINT_POLICY_LABEL)
                            .collect();
                        let [policy] = policies.as_slice() else {
                            return Err(TestError::Unexpected(format!(
                                "{label} {resolution:?}: erwartet genau eine DoD-Taint-Politik \
                                 in der Kette, gefunden {}",
                                policies.len()
                            )));
                        };
                        assert!(
                            chain
                                .snapshot()
                                .contains(&(DOD_TAINT_POLICY_LABEL, ApprovalHandlerKind::Other)),
                            "{label} {resolution:?}: {:?}",
                            chain.snapshot()
                        );
                        let decision = with_review_taint(true, policy.review(&stop)).await;
                        assert_eq!(
                            matches!(decision, ApprovalDecision::AskUser(_)),
                            mode != ApprovalMode::FullAccess,
                            "{label} {resolution:?} {mode:?}: {decision:?}"
                        );
                    }
                }
            }
            Ok(())
        }

        /// Auto-Modus mit einer gelernten Allow-Regel für `job.stop` in der
        /// echten Wurzelkette: ohne DoD-Abfrage läuft `job.stop` durch; nach
        /// `dod.findings` im selben Turn pausiert der Turn vor `job.stop`, und
        /// die Oberfläche bekommt den Taint-Grund.
        #[tokio::test]
        async fn the_real_root_chain_asks_after_a_dod_query_despite_an_allow_rule() -> TestResult {
            let chain = root_chain(
                AskResolution::Interactive,
                ApprovalMode::Delegated,
                job_stop_allowed()?,
            );

            let mut untainted = session_on(&chain)?;
            let outcome = run(
                &mut untainted,
                vec![
                    respond(vec![call("job.stop")]),
                    ModelResponse::text("gestoppt"),
                ],
            )
            .await?;
            // Steht `job.stop` einmal in `ALWAYS_ASK_TOOLS` (Phase 3.5),
            // fragt es schon ohne Taint.
            if harw_registry_defaults::ALWAYS_ASK_TOOLS.contains(&"job.stop") {
                assert!(
                    matches!(outcome, TurnOutcome::AwaitingApproval { .. }),
                    "{outcome:?}"
                );
            } else {
                assert!(matches!(outcome, TurnOutcome::Completed), "{outcome:?}");
                let results = tool_results(&untainted);
                assert!(
                    matches!(results.as_slice(), [ToolCallResult::Success { .. }]),
                    "{results:?}"
                );
            }

            let mut tainted = session_on(&chain)?;
            let stop = call("job.stop");
            let stop_id = stop.id.clone();
            let outcome = run(
                &mut tainted,
                vec![
                    respond(vec![call("dod.findings")]),
                    respond(vec![stop]),
                    ModelResponse::text("erledigt"),
                ],
            )
            .await?;
            match &outcome {
                TurnOutcome::AwaitingApproval { call_id, .. } if *call_id == stop_id => {}
                other => {
                    return Err(TestError::Unexpected(format!(
                        "erwartet die Rückfrage für job.stop, bekommen {other:?}"
                    )));
                }
            }
            let reason = pending_ask_reason(&tainted)
                .ok_or(TestError::Missing("Taint-Grund der offenen Rückfrage"))?;
            assert!(reason.contains(TAINT_REASON), "{reason}");
            Ok(())
        }

        /// Entscheidung O1 Ende zu Ende: unter Full Access fragt die echte
        /// Kette nach `dod.findings` nie und lehnt nichts ab — in der
        /// interaktiven Wurzel, in jedem nicht interaktiven Einstieg und im
        /// Kind. `job.stop`, `process.kill` und `host.sudo_exec` laufen durch.
        #[tokio::test]
        async fn full_access_never_asks_or_denies_after_a_dod_query() -> TestResult {
            for resolution in RESOLUTIONS {
                let root = root_chain(resolution, ApprovalMode::FullAccess, AllowRuleSet::new());
                for (label, chain) in [("Wurzel", root.clone()), ("Kind", root.for_child())] {
                    let mut session = session_on(&chain)?;
                    let outcome = run(
                        &mut session,
                        vec![
                            respond(vec![call("dod.findings")]),
                            respond(vec![
                                call("job.stop"),
                                call("process.kill"),
                                call("host.sudo_exec"),
                            ]),
                            ModelResponse::text("erledigt"),
                        ],
                    )
                    .await?;
                    assert!(
                        matches!(outcome, TurnOutcome::Completed),
                        "{label} {resolution:?}: {outcome:?}"
                    );
                    let results = tool_results(&session);
                    assert_eq!(results.len(), 4, "{label} {resolution:?}: {results:?}");
                    assert!(
                        results
                            .iter()
                            .all(|result| matches!(result, ToolCallResult::Success { .. })),
                        "{label} {resolution:?}: {results:?}"
                    );
                    assert_eq!(pending_ask_reason(&session), None, "{label} {resolution:?}");
                }
            }
            Ok(())
        }

        /// Ein nicht interaktiver Einstieg bekommt nach einer DoD-Abfrage eine
        /// deterministische Ablehnung mit dem Taint-Grund statt einer Pause,
        /// die niemand auflöst — auch gegen eine Allow-Regel; der Turn läuft
        /// weiter.
        #[tokio::test]
        async fn a_non_interactive_entry_denies_a_tainted_job_stop_with_the_reason() -> TestResult {
            for resolution in [
                AskResolution::RejectTurn,
                AskResolution::BlockJob,
                AskResolution::Fail,
            ] {
                let chain = root_chain(resolution, ApprovalMode::Delegated, job_stop_allowed()?);
                let mut session = session_on(&chain)?;
                let outcome = run(
                    &mut session,
                    vec![
                        respond(vec![call("dod.findings")]),
                        respond(vec![call("job.stop")]),
                        ModelResponse::text("erledigt"),
                    ],
                )
                .await?;
                assert!(
                    matches!(outcome, TurnOutcome::Completed),
                    "{resolution:?}: {outcome:?}"
                );
                match tool_results(&session).as_slice() {
                    [
                        ToolCallResult::Success { .. },
                        ToolCallResult::Error { message },
                    ] if message.starts_with("denied: ") && message.contains(TAINT_REASON) => {}
                    other => {
                        return Err(TestError::Unexpected(format!(
                            "{resolution:?}: erwartet DoD-Erfolg und Taint-Ablehnung, \
                             bekommen {other:?}"
                        )));
                    }
                }
            }
            Ok(())
        }
    }
}
