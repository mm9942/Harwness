//! `/permissions rules`, `/permissions log` und die Allow-Sperre für
//! `ALWAYS_ASK_TOOLS` (Runde 5, Teil E).
//!
//! # Verantwortlichkeit
//! - [`list_rules`] — die geltenden Allow-/Deny-Regeln mit Herkunft
//!   (Sitzung, Projekt, Global) und Nummer für `/permissions rm <nr>`.
//! - [`decision_log`] — die letzten Entscheidungen des Auto-Modus aus dem
//!   geteilten [`AutoDecisionLog`] samt Stand des Sicherheitsdeckels.
//! - [`reject_always_ask_allow`] — lehnt eine Allow-Regel für ein Werkzeug
//!   aus `ALWAYS_ASK_TOOLS` ab: sie wäre wirkungslos (die Standardpolitik
//!   fragt dort immer) und täuschte eine Freigabe nur vor.

use harw_extension_api::allow_rules::{AllowRuleSet, RuleDecision};
use harw_extension_api::auto_mode::{
    AutoDecision, AutoDecisionLog, CONSECUTIVE_DENIAL_CAP, TOTAL_DENIAL_CAP,
};
use harw_operations::{OpContext, OpError, OpOutput};
use harw_registry_defaults::ALWAYS_ASK_TOOLS;

use super::{NO_ALLOW_RULE_SET, decision_label_de, scope_label_de};

/// Meldung, wenn kein [`AutoDecisionLog`] registriert ist.
pub(crate) const NO_AUTO_DECISION_LOG: &str = "In dieser Laufzeit ist kein Auto-Modus-Protokoll registriert — \
     der ausgebaute Auto-Modus ist hier nicht aktiv (nur interaktive Sitzungen führen ihn).";

/// Voreingestellte Anzahl Einträge für `/permissions log`.
const DEFAULT_LOG_ENTRIES: usize = 20;

/// Höchstzahl Einträge für `/permissions log`.
const MAX_LOG_ENTRIES: usize = 100;

/// Lehnt eine Allow-Regel für ein `ALWAYS_ASK_TOOLS`-Werkzeug ab.
///
/// # Errors
/// [`OpError::InvalidArguments`], wenn `tool` exakt ein Werkzeug aus
/// `ALWAYS_ASK_TOOLS` ist.
pub(super) fn reject_always_ask_allow(tool: &str) -> Result<(), OpError> {
    if ALWAYS_ASK_TOOLS.contains(&tool) {
        return Err(OpError::InvalidArguments(format!(
            "`{tool}` fragt immer nach (auch im Auto-Modus und unter `full`) — \
             eine Allow-Regel ist dafür nicht möglich."
        )));
    }
    Ok(())
}

/// `/permissions rules`: die Regeln mit Herkunft.
///
/// # Errors
/// [`OpError::NotAvailable`], wenn kein [`AllowRuleSet`] registriert ist.
pub(super) fn list_rules(ctx: &OpContext) -> Result<OpOutput, OpError> {
    let Some(rule_set) = ctx.service::<AllowRuleSet>() else {
        return Err(OpError::NotAvailable(NO_ALLOW_RULE_SET.to_owned()));
    };
    let rules = rule_set.snapshot();
    let body = if rules.is_empty() {
        "  (keine Regeln)".to_owned()
    } else {
        rules
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let marker = match rule.decision {
                    RuleDecision::Deny => "✗",
                    RuleDecision::Allow => "✓",
                };
                format!(
                    "  {}. {marker} {} {} → {} (Herkunft: {})",
                    index + 1,
                    rule.tool,
                    rule.pattern.as_deref().unwrap_or("*"),
                    decision_label_de(rule.decision),
                    scope_label_de(rule.scope),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let always_ask = ALWAYS_ASK_TOOLS.join(", ");
    Ok(OpOutput::from(format!(
        "Regeln (Deny schlägt Allow; Reihenfolge: Deny → Allow → Auto-Klassifizierer):\n{body}\n\n\
         Immer erfragt, unabhängig von Regeln und Modus: {always_ask}\n\
         Entfernen mit `/permissions rm <nr>`; hinzufügen mit \
         `/permissions allow|deny <tool> [muster] [--session|--project|--user]`."
    )))
}

/// `/permissions log [anzahl]`: die letzten Auto-Modus-Entscheidungen.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: `anzahl` ist keine Zahl ≥ 1.
/// - [`OpError::NotAvailable`]: kein [`AutoDecisionLog`] registriert.
pub(super) fn decision_log(ctx: &OpContext, tail: &[String]) -> Result<OpOutput, OpError> {
    let count = match tail.first() {
        None => DEFAULT_LOG_ENTRIES,
        Some(raw) => match raw.parse::<usize>() {
            Ok(value) if value >= 1 => value.min(MAX_LOG_ENTRIES),
            _ => {
                return Err(OpError::InvalidArguments(format!(
                    "/permissions log [anzahl] erwartet eine Zahl ≥ 1, war `{raw}`"
                )));
            }
        },
    };
    let Some(log) = ctx.service::<AutoDecisionLog>() else {
        return Err(OpError::NotAvailable(NO_AUTO_DECISION_LOG.to_owned()));
    };
    let (consecutive, total) = log.denial_counters();
    let cap = if log.is_tripped() {
        " — Deckel ausgelöst, Modus steht auf „ask“"
    } else {
        ""
    };
    let header = format!(
        "Auto-Modus — Ablehnungen in Folge {consecutive}/{CONSECUTIVE_DENIAL_CAP}, \
         insgesamt {total}/{TOTAL_DENIAL_CAP}{cap}"
    );
    let entries = log.recent(count);
    if entries.is_empty() {
        return Ok(OpOutput::from(format!(
            "{header}\n  (noch keine Entscheidungen)"
        )));
    }
    let lines = entries
        .iter()
        .rev()
        .map(|entry| {
            let time = entry
                .at
                .to_zoned(jiff::tz::TimeZone::system())
                .strftime("%H:%M:%S")
                .to_string();
            let label = match entry.verdict.decision {
                AutoDecision::Allow => "✓ erlaubt ",
                AutoDecision::Ask => "? gefragt ",
                AutoDecision::Deny => "✗ abgelehnt",
            };
            format!(
                "  {time}  {label}  {}  — {}: {} [{}]",
                single_line(&entry.summary),
                single_line(&entry.verdict.category),
                single_line(&entry.verdict.reason),
                entry.verdict.source.as_str(),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(OpOutput::from(format!(
        "{header}\nLetzte {} Entscheidungen (neueste zuerst):\n{lines}",
        entries.len()
    )))
}

/// Ersetzt Steuerzeichen durch Leerzeichen (Protokolltexte stammen teils
/// vom Modell).
fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
