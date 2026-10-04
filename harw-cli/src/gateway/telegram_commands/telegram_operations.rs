//! Macro-/Registry-getriebene Telegram-Projektion der Harwness-Slash-Operations.
//!
//! Telegram ist hier nur eine Darstellung der kanalfähigen
//! `Surface::Command`-Metadaten. Autorisierung passiert weiterhin beim
//! Dispatch über `CommandAdapter`, Principal und Sandbox.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use harw_channel_telegram_transport::BotCommand;
use harw_operations::adapter::CommandAdapter;
use harw_operations::operation::CommandVisibility;
use harw_operations::registry::OperationRegistry;
const TELEGRAM_COMMAND_MAX_BYTES: usize = 32;
const TELEGRAM_DESCRIPTION_MAX_CHARS: usize = 256;

/// Ein auf Telegram projizierter Harwness-Command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TelegramOperationSpec {
    /// Telegram-tauglicher Name ohne führenden Slash.
    pub telegram_name: String,
    /// Verlustfreier kanonischer Harwness-Pfad, z. B. `/context-proposal`.
    pub canonical_path: String,
    /// Kurze Beschreibung aus `OperationMeta::summary`.
    pub description: String,
}

/// Verlustfreie direkte Ausführung einer kanalfähigen Operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TelegramOperationInvocation {
    pub canonical_path: String,
    pub args: Vec<String>,
}

/// Ergebnis der Telegram-spezifischen Slash-Auflösung.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OperationResolve {
    NoMatch,
    Invalid(String),
    Invocation(TelegramOperationInvocation),
}

/// Deterministische Projektion aller kanalfähigen Operationen.
#[derive(Clone, Debug, Default)]
pub(crate) struct TelegramOperationCatalog {
    specs: Arc<[TelegramOperationSpec]>,
    by_alias: Arc<BTreeMap<String, usize>>,
    by_canonical: Arc<BTreeMap<String, usize>>,
}

impl TelegramOperationCatalog {
    /// Baut den Katalog direkt aus der OperationRegistry.
    ///
    /// `reserved` sind Telegram-native Gateway-Befehle. Bei Kollisionen
    /// erhält die generische Operation deterministisch ein `harw_`-Alias.
    pub fn from_registry(registry: &OperationRegistry, reserved: &[&str]) -> Self {
        let mut used = reserved
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<BTreeSet<_>>();
        let mut rows = Vec::new();

        for operation in registry.iter() {
            let summary = operation.meta().summary;
            for adapter in CommandAdapter::from_operation(Arc::clone(operation)) {
                if adapter.visibility() == CommandVisibility::TuiOnly {
                    continue;
                }
                let canonical = adapter.path().to_owned();
                let base = normalize_telegram_name(adapter.path());
                if base.is_empty() {
                    continue;
                }
                let mut alias = fit_telegram_name(&base, &canonical);
                if used.contains(&alias) {
                    alias = fit_telegram_name(&format!("harw_{base}"), &canonical);
                }
                if used.contains(&alias) {
                    let suffix = stable_suffix(&canonical);
                    let prefix_len = TELEGRAM_COMMAND_MAX_BYTES.saturating_sub(1 + suffix.len());
                    let prefix = alias.chars().take(prefix_len).collect::<String>();
                    alias = format!("{prefix}_{suffix}");
                }
                if used.contains(&alias) {
                    // Nur bei zwei identischen Command-Surfaces derselben
                    // kanonischen Operation; der erste Eintrag genügt.
                    continue;
                }
                used.insert(alias.clone());
                rows.push(TelegramOperationSpec {
                    telegram_name: alias,
                    canonical_path: canonical,
                    description: telegram_description(summary, adapter.operation_name()),
                });
            }
        }

        rows.sort_by(|left, right| {
            left.telegram_name
                .cmp(&right.telegram_name)
                .then_with(|| left.canonical_path.cmp(&right.canonical_path))
        });
        let mut by_alias = BTreeMap::new();
        let mut by_canonical = BTreeMap::new();
        for (index, row) in rows.iter().enumerate() {
            by_alias.insert(row.telegram_name.clone(), index);
            by_canonical
                .entry(row.canonical_path.clone())
                .or_insert(index);
        }
        Self {
            specs: rows.into(),
            by_alias: Arc::new(by_alias),
            by_canonical: Arc::new(by_canonical),
        }
    }

    #[must_use]
    pub fn specs(&self) -> &[TelegramOperationSpec] {
        &self.specs
    }

    /// Menüeinträge, begrenzt auf die von Telegram verbleibende Kapazität.
    #[must_use]
    pub fn menu_commands(&self, capacity: usize) -> Vec<BotCommand> {
        self.specs
            .iter()
            .take(capacity)
            .map(|spec| BotCommand {
                command: spec.telegram_name.clone(),
                description: spec.description.clone(),
            })
            .collect()
    }

    /// Löst einen Telegram-Slash-Text in den kanonischen Harwness-Aufruf auf.
    ///
    /// Zusätzlich zu projizierten Aliasen unterstützt `/op <pfad> ...` den
    /// verlustfreien Zugriff auf jeden kanalfähigen kanonischen Pfad.
    pub fn resolve(&self, text: &str) -> OperationResolve {
        let mut tokens = text.split_ascii_whitespace();
        let Some(head) = tokens.next() else {
            return OperationResolve::NoMatch;
        };
        let Some(raw_name) = head.strip_prefix('/') else {
            return OperationResolve::NoMatch;
        };
        let name = raw_name
            .split_once('@')
            .map_or(raw_name, |(name, _)| name)
            .to_ascii_lowercase();
        if name == "op" {
            let Some(path) = tokens.next() else {
                return OperationResolve::Invalid(
                    "Verwendung: /op <harw-befehl> [argumente…]".to_owned(),
                );
            };
            let canonical = canonical_path(path);
            let Some(index) = self.by_canonical.get(&canonical).copied() else {
                return OperationResolve::Invalid(format!(
                    "Harwness-Befehl {canonical} ist über Telegram nicht verfügbar."
                ));
            };
            let spec = &self.specs[index];
            return OperationResolve::Invocation(TelegramOperationInvocation {
                canonical_path: spec.canonical_path.clone(),
                args: tokens.map(str::to_owned).collect(),
            });
        }

        let Some(index) = self.by_alias.get(&name).copied() else {
            return OperationResolve::NoMatch;
        };
        let spec = &self.specs[index];
        OperationResolve::Invocation(TelegramOperationInvocation {
            canonical_path: spec.canonical_path.clone(),
            args: tokens.map(str::to_owned).collect(),
        })
    }
}

fn canonical_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

fn normalize_telegram_name(path: &str) -> String {
    let mut out = String::new();
    let mut underscore = false;
    for byte in path.trim_start_matches('/').bytes() {
        let mapped = match byte {
            b'a'..=b'z' | b'0'..=b'9' | b'_' => Some(byte as char),
            b'A'..=b'Z' => Some((byte + 32) as char),
            _ => None,
        };
        match mapped {
            Some(ch) => {
                out.push(ch);
                underscore = false;
            }
            None if !underscore && !out.is_empty() => {
                out.push('_');
                underscore = true;
            }
            None => {}
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

fn fit_telegram_name(name: &str, stable_key: &str) -> String {
    if name.len() <= TELEGRAM_COMMAND_MAX_BYTES {
        return name.to_owned();
    }
    let suffix = stable_suffix(stable_key);
    let prefix_len = TELEGRAM_COMMAND_MAX_BYTES.saturating_sub(1 + suffix.len());
    let prefix = name.chars().take(prefix_len).collect::<String>();
    format!("{prefix}_{suffix}")
}

fn stable_suffix(value: &str) -> String {
    // FNV-1a: klein, vollständig deterministisch und für einen Namenssuffix
    // ausreichend; keine Sicherheitsfunktion.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:08x}", hash as u32)
}

fn telegram_description(summary: &str, operation_name: &str) -> String {
    let trimmed = summary.trim();
    let source = if trimmed.is_empty() {
        operation_name
    } else {
        trimmed
    };
    let mut value = source
        .chars()
        .take(TELEGRAM_DESCRIPTION_MAX_CHARS)
        .collect::<String>();
    if value.is_empty() {
        value.push_str("Harwness operation");
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_projection_is_telegram_safe_and_bounded() {
        assert_eq!(normalize_telegram_name("/context-proposal"), "context_proposal");
        assert_eq!(normalize_telegram_name("/Agent/STATUS"), "agent_status");
        let fitted = fit_telegram_name(
            "this_command_name_is_much_too_long_for_telegram",
            "/this-command-name-is-much-too-long-for-telegram",
        );
        assert!(fitted.len() <= TELEGRAM_COMMAND_MAX_BYTES);
        assert!(
            fitted
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        );
    }

    #[test]
    fn stable_suffix_is_stable_and_path_sensitive() {
        assert_eq!(stable_suffix("/a"), stable_suffix("/a"));
        assert_ne!(stable_suffix("/a"), stable_suffix("/b"));
    }
}
