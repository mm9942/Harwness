//! Gemeinsame Grenzen und Politik der nativen Reasoning-Replays.
//!
//! Die drei Replay-Caches (`responses_reasoning`, `chat_reasoning`,
//! `anthropic_replay`) halten native Assistant-Runden (verschlüsseltes
//! Reasoning, signierte Thinking-Blöcke, `reasoning_content`) gebunden an
//! Identität, Modell und exakten Verlaufspräfix.
//!
//! Früher teilten sich alle Sessions einer Provider-Instanz (Root-Orchestrator
//! samt aller Kinder) **64 Runden insgesamt**; jede nicht mehr abgedeckte
//! Runde brach den Request hart ab. Lange Orchestrator-Turns, Kompaktierung,
//! automatische Fortsetzungen nach `MaxTokens` und Neustarts führten so zu
//! „continuity unavailable"-Abbrüchen. Dieses Modul bündelt
//! - die Verdrängung **je Identität** ([`evict_for`]) und
//! - die Politik bei fehlender Kontinuität ([`ReplayPolicy`]): Vorgabe ist,
//!   die Runde ohne natives Reasoning zu senden und das zu protokollieren.

use std::collections::{BTreeSet, VecDeque};

use harw_core::{ModelError, RequestIdentity};

/// Höchstzahl gehaltener Runden je Identität (Session + Agent).
pub(crate) const MAX_ROUNDS_PER_IDENTITY: usize = 64;

/// Höchstzahl gehaltener Runden je Provider-Instanz über alle Identitäten.
pub(crate) const MAX_ROUNDS_TOTAL: usize = 1024;

/// Verhalten bei fehlender nativer Reasoning-Kontinuität.
///
/// # Concurrency
/// `Copy`, zustandslos.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ReplayPolicy {
    /// Ohne natives Reasoning weitersenden und warnen (Vorgabe).
    #[default]
    Degrade,
    /// Mit [`ModelError::ReplayUnavailable`] abbrechen.
    Strict,
}

impl ReplayPolicy {
    /// Politik für `model` aus der Menge der als `strict` konfigurierten
    /// Modelle (IDs und Aliase).
    #[must_use]
    pub(crate) fn for_model(strict_models: &BTreeSet<String>, model: &str) -> Self {
        if strict_models.contains(model) {
            Self::Strict
        } else {
            Self::Degrade
        }
    }
}

/// Baut den typisierten Fehler einer fehlenden Kontinuität.
#[must_use]
pub(crate) fn unavailable(family: &str, detail: &str) -> ModelError {
    ModelError::ReplayUnavailable {
        provider: family.to_owned(),
        detail: format!(
            "{detail}; start a fresh conversation, rebuild its context, or set \
             `[capabilities] reasoning_replay = \"degrade\"` for this model"
        ),
    }
}

/// Entscheidet über eine fehlende Kontinuität: `Ok(())` heißt „ohne natives
/// Reasoning weitersenden" (protokolliert), sonst der typisierte Fehler.
///
/// # Errors
/// [`ModelError::ReplayUnavailable`] unter [`ReplayPolicy::Strict`].
pub(crate) fn on_missing(
    policy: ReplayPolicy,
    family: &str,
    model: &str,
    detail: &str,
    uncovered: usize,
) -> Result<(), ModelError> {
    match policy {
        ReplayPolicy::Strict => Err(unavailable(family, detail)),
        ReplayPolicy::Degrade => {
            tracing::warn!(
                target: "harw.replay",
                code = "HARW-REPLAY-002",
                family,
                model,
                detail,
                uncovered,
                "reasoning replay degraded: sending affected rounds without native reasoning"
            );
            Ok(())
        }
    }
}

/// Protokolliert eine nicht gemerkte Runde, ohne die bereits erhaltene
/// Antwort zu verwerfen.
///
/// # Description
/// Ein Fehler beim Merken darf nie eine erfolgreiche (und bezahlte)
/// Modellantwort vernichten; die nächste Anfrage entscheidet über
/// [`on_missing`], ob sie ohne diese Runde weitersendet.
pub(crate) fn remember_or_warn(family: &str, model: &str, result: Result<(), ModelError>) {
    if let Err(error) = result {
        tracing::warn!(
            target: "harw.replay",
            code = "HARW-REPLAY-003",
            family,
            model,
            error = %error,
            "native reasoning round could not be retained"
        );
    }
}

/// Macht Platz für einen neuen Eintrag von `identity` mit `bytes` Bytes.
///
/// # Description
/// Verdrängt zuerst die ältesten Einträge **derselben** Identität über
/// [`MAX_ROUNDS_PER_IDENTITY`] hinaus, danach global die ältesten Einträge,
/// solange [`MAX_ROUNDS_TOTAL`] oder `max_bytes` überschritten würden. Eine
/// Session mit vielen Runden verdrängt so nicht mehr die Runden des
/// Root-Orchestrators oder seiner Geschwister.
pub(crate) fn evict_for<T>(
    entries: &mut VecDeque<T>,
    identity_of: impl Fn(&T) -> &RequestIdentity,
    bytes_of: impl Fn(&T) -> usize,
    identity: &RequestIdentity,
    bytes: usize,
    max_bytes: usize,
) {
    loop {
        let own = entries
            .iter()
            .filter(|entry| identity_of(entry) == identity)
            .count();
        if own < MAX_ROUNDS_PER_IDENTITY {
            break;
        }
        let Some(oldest) = entries
            .iter()
            .position(|entry| identity_of(entry) == identity)
        else {
            break;
        };
        entries.remove(oldest);
    }
    while !entries.is_empty()
        && (entries.len() >= MAX_ROUNDS_TOTAL
            || entries.iter().map(&bytes_of).sum::<usize>() + bytes > max_bytes)
    {
        entries.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(session: &str) -> RequestIdentity {
        RequestIdentity {
            session: session.to_owned(),
            agent: "agent".to_owned(),
            role: "worker".to_owned(),
        }
    }

    #[test]
    fn a_busy_identity_evicts_only_its_own_rounds() {
        let root = identity("root");
        let child = identity("child");
        let mut entries: VecDeque<(RequestIdentity, usize)> = VecDeque::new();
        entries.push_back((root.clone(), 1));
        for _ in 0..(MAX_ROUNDS_PER_IDENTITY * 3) {
            evict_for(&mut entries, |e| &e.0, |e| e.1, &child, 1, usize::MAX);
            entries.push_back((child.clone(), 1));
        }
        assert!(entries.iter().any(|entry| entry.0 == root));
        assert_eq!(
            entries.iter().filter(|entry| entry.0 == child).count(),
            MAX_ROUNDS_PER_IDENTITY
        );
    }

    #[test]
    fn the_total_and_byte_limits_still_apply() {
        let mut entries: VecDeque<(RequestIdentity, usize)> = VecDeque::new();
        for index in 0..(MAX_ROUNDS_TOTAL + 10) {
            let id = identity(&format!("s{index}"));
            evict_for(&mut entries, |e| &e.0, |e| e.1, &id, 1, usize::MAX);
            entries.push_back((id, 1));
        }
        assert_eq!(entries.len(), MAX_ROUNDS_TOTAL);
        let id = identity("big");
        evict_for(&mut entries, |e| &e.0, |e| e.1, &id, 10, 12);
        assert!(entries.iter().map(|e| e.1).sum::<usize>() + 10 <= 12);
    }

    #[test]
    fn strict_fails_with_the_stable_code_and_degrade_continues() {
        let strict: BTreeSet<String> = ["m".to_owned()].into_iter().collect();
        assert_eq!(ReplayPolicy::for_model(&strict, "m"), ReplayPolicy::Strict);
        assert_eq!(ReplayPolicy::for_model(&strict, "x"), ReplayPolicy::Degrade);
        let error = on_missing(ReplayPolicy::Strict, "chat", "m", "lost", 1);
        assert!(
            error
                .as_ref()
                .is_err_and(|e| e.to_string().contains(harw_core::REPLAY_UNAVAILABLE_CODE))
        );
        assert!(on_missing(ReplayPolicy::Degrade, "chat", "m", "lost", 1).is_ok());
    }
}
