//! Auto-Modus in der TUI: Werkzeugzellen-Vermerke und Lern-Angebot
//! (Runde 5, Teil E).
//!
//! # Verantwortung
//! Reine Darstellungs- und Übersetzungslogik, ohne eigenen Zustand:
//! - [`auto_note_for`] — der kompakte Vermerk an der Werkzeugzelle:
//!   „auto ✓ <Grund>“ für automatisch freigegebene, „Vom Auto-Modus
//!   abgelehnt · <Kategorie>“ für abgelehnte Aufrufe.
//! - [`auto_ask_reason_for`] — Runde 6, Teil A1: der Grund, aus dem der
//!   Auto-Modus die Person fragt („<Kategorie> – <Grund>“); der
//!   Freigabedialog zeigt ihn als Zeile „Auto-Modus: …“.
//! - [`LearnOfferView`] — das Lern-Angebot des Freigabedialogs („Ja, und
//!   künftig erlauben: `<muster>`“) mit Scope-Wahl Sitzung oder Projekt,
//!   dazu die anzulegende Regel ([`LearnOfferView::rule`]) und der
//!   `/permissions allow …`-Befehl, über den die Projekt-Regel dauerhaft
//!   geschrieben wird ([`LearnOfferView::persist_command`]).
//!
//! Die Einhängepunkte liegen in `app.rs` (Werkzeugzelle, Freigabedialog,
//! Sitzungsziel) und `approval_dialog.rs` (Optionen); die Zähl- und
//! Sicherheitslogik liegt in `harw-runtime` (`auto_classifier.rs`,
//! `permission_rules.rs`).
//!
//! # Terminal-Sicherheit
//! Kategorie und Grund stammen vom Klassifizierer-Modell; sie laufen durch
//! [`crate::sanitize::sanitize_inline`] und werden gekürzt.

use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope};
use harw_extension_api::auto_mode::{AUTO_DENIAL_PREFIX, AutoDecision, AutoVerdict};
use harw_runtime::LearnOffer;

use crate::sanitize::sanitize_inline;

/// Höchstlänge (Zeichen) eines Grundes im Zellvermerk.
const MAX_NOTE_REASON_CHARS: usize = 80;

/// Wo eine gelernte Regel gilt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LearnScope {
    /// Nur diese Sitzung (geteilte Regelmenge, nichts auf der Platte).
    Session,
    /// Dauerhaft im Projekt (über `/permissions allow … --project`).
    Project,
}

impl LearnScope {
    /// Die Regel-Lebensdauer.
    #[must_use]
    pub fn rule_scope(self) -> RuleScope {
        match self {
            Self::Session => RuleScope::Session,
            Self::Project => RuleScope::Project,
        }
    }
}

/// Das Lern-Angebot, wie der Freigabedialog es zeigt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnOfferView {
    /// Werkzeugname.
    pub tool: String,
    /// Regel-Muster (`None` = jeder Aufruf).
    pub pattern: Option<String>,
    /// Anzeigeform (`shell.exec cargo test *`).
    pub display: String,
}

impl From<&LearnOffer> for LearnOfferView {
    fn from(offer: &LearnOffer) -> Self {
        Self {
            tool: offer.key.tool.clone(),
            pattern: offer.key.pattern.clone(),
            display: offer.key.display(),
        }
    }
}

impl LearnOfferView {
    /// Die Beschriftung der Dialog-Option.
    #[must_use]
    pub fn option_label(&self, scope: LearnScope) -> String {
        let suffix = match scope {
            LearnScope::Session => "nur diese Sitzung",
            LearnScope::Project => "dauerhaft im Projekt",
        };
        format!(
            "Ja, und künftig erlauben: {} ({suffix})",
            sanitize_inline(&self.display)
        )
    }

    /// Die anzulegende Allow-Regel.
    #[must_use]
    pub fn rule(&self, scope: LearnScope) -> ApprovalRule {
        ApprovalRule {
            tool: self.tool.clone(),
            pattern: self.pattern.clone(),
            decision: RuleDecision::Allow,
            scope: scope.rule_scope(),
        }
    }

    /// Der synthetische Befehl, der die Projekt-Regel über den bestehenden
    /// `/permissions allow`-Pfad (und damit den `ConfigWriter`) schreibt.
    ///
    /// # Arguments
    /// - `quote` (`impl Fn(&str) -> String`): Quotierung für mehrwortige
    ///   Muster (in `app.rs`: `quote_for_synthetic_command`).
    #[must_use]
    pub fn persist_command(&self, quote: impl Fn(&str) -> String) -> String {
        match &self.pattern {
            Some(pattern) => format!(
                "/permissions allow {} {} --project",
                self.tool,
                quote(pattern)
            ),
            None => format!("/permissions allow {} --project", self.tool),
        }
    }
}

/// Der Vermerk an der Werkzeugzelle für ein Auto-Modus-Urteil.
///
/// # Rückgabe
/// `Some("auto ✓ <Grund>")` bei `allow`, `Some("Vom Auto-Modus abgelehnt ·
/// <Kategorie>")` bei `deny`; `None` bei `ask` (dann entscheidet die Person,
/// und der Freigabedialog setzt seinen eigenen Vermerk).
#[must_use]
pub fn auto_note_for(verdict: &AutoVerdict) -> Option<String> {
    match verdict.decision {
        AutoDecision::Allow => Some(format!(
            "auto ✓ {}",
            truncate(&sanitize_inline(&verdict.reason), MAX_NOTE_REASON_CHARS)
        )),
        AutoDecision::Deny => Some(format!(
            "{AUTO_DENIAL_PREFIX} · {}",
            truncate(&sanitize_inline(&verdict.category), MAX_NOTE_REASON_CHARS)
        )),
        AutoDecision::Ask => None,
    }
}

/// Höchstlänge (Zeichen) des Auto-Modus-Grundes im Freigabedialog.
const MAX_DIALOG_REASON_CHARS: usize = 240;

/// Runde 6, Teil A1: der Grund einer Auto-Modus-Rückfrage für den
/// Freigabedialog.
///
/// # Beschreibung
/// Nur für `ask`-Urteile (auch aus `deny` umgewandelte); bereinigt
/// Steuerzeichen und kürzt. Der Dialog stellt „Auto-Modus: “ voran.
///
/// # Rückgabe
/// `Some("<Kategorie> – <Grund>")` bei `ask`, sonst `None`.
#[must_use]
pub fn auto_ask_reason_for(verdict: &AutoVerdict) -> Option<String> {
    (verdict.decision == AutoDecision::Ask).then(|| {
        truncate(
            &sanitize_inline(&verdict.dialog_reason()),
            MAX_DIALOG_REASON_CHARS,
        )
    })
}

/// Kürzt zeichensicher mit `…`.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_extension_api::auto_mode::VerdictSource;
    use harw_runtime::LearnKey;

    fn offer(tool: &str, pattern: Option<&str>) -> LearnOfferView {
        LearnOfferView::from(&LearnOffer {
            key: LearnKey {
                tool: tool.to_owned(),
                pattern: pattern.map(str::to_owned),
            },
            approvals: 2,
        })
    }

    #[test]
    fn allow_and_deny_get_their_cell_notes_ask_gets_none() {
        let allow = AutoVerdict::new(
            AutoDecision::Allow,
            "test-run",
            "Tests laufen lassen",
            VerdictSource::Classifier,
        );
        assert_eq!(
            auto_note_for(&allow).as_deref(),
            Some("auto ✓ Tests laufen lassen")
        );
        let deny = AutoVerdict::new(
            AutoDecision::Deny,
            "exfiltration",
            "lädt hoch",
            VerdictSource::Classifier,
        );
        assert_eq!(
            auto_note_for(&deny).as_deref(),
            Some("Vom Auto-Modus abgelehnt · exfiltration")
        );
        let ask = AutoVerdict::fallback_ask("x");
        assert_eq!(auto_note_for(&ask), None);
    }

    #[test]
    fn hostile_model_text_is_sanitized_in_the_note() {
        let verdict = AutoVerdict::new(
            AutoDecision::Allow,
            "c",
            "ok\u{1b}[2J\nzweite Zeile",
            VerdictSource::Classifier,
        );
        let note = auto_note_for(&verdict).unwrap_or_default();
        assert!(!note.contains('\u{1b}'), "{note:?}");
        assert!(!note.contains('\n'), "{note:?}");
    }

    /// Runde 6, Teil A1: ein aus `deny` umgewandeltes `ask` liefert den
    /// Grund für den Dialog; `allow`/`deny` liefern keinen.
    #[test]
    fn ask_verdicts_yield_a_dialog_reason() {
        let escalated = AutoVerdict::new(
            AutoDecision::Deny,
            "exfiltration",
            "verschiebt nach ~\u{1b}[2J",
            VerdictSource::Classifier,
        )
        .escalate_to_ask();
        let reason = auto_ask_reason_for(&escalated).unwrap_or_default();
        assert!(
            reason.starts_with("exfiltration – verschiebt nach ~"),
            "{reason}"
        );
        assert!(!reason.contains('\u{1b}'), "{reason:?}");
        let allow = AutoVerdict::new(AutoDecision::Allow, "c", "r", VerdictSource::Classifier);
        assert_eq!(auto_ask_reason_for(&allow), None);
        let deny = AutoVerdict::new(AutoDecision::Deny, "c", "r", VerdictSource::Classifier);
        assert_eq!(auto_ask_reason_for(&deny), None);
    }

    #[test]
    fn learn_offer_builds_labels_rules_and_persist_command() {
        let view = offer("shell.exec", Some("cargo test"));
        assert_eq!(
            view.option_label(LearnScope::Session),
            "Ja, und künftig erlauben: shell.exec cargo test * (nur diese Sitzung)"
        );
        assert_eq!(view.rule(LearnScope::Project).scope, RuleScope::Project);
        assert_eq!(view.rule(LearnScope::Session).decision, RuleDecision::Allow);
        assert_eq!(
            view.persist_command(|pattern| format!("\"{pattern}\"")),
            "/permissions allow shell.exec \"cargo test\" --project"
        );
        assert_eq!(
            offer("mcp.lookup", None).persist_command(|p| p.to_owned()),
            "/permissions allow mcp.lookup --project"
        );
    }
}
