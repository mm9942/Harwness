//! Warden-eigene Durchsetzungs-Policy: [`WardenPolicy`], [`ActionAllowlist`],
//! [`DenyList`] (PL-100 WP 3.1, Leitprinzip I7, Bedrohung B12).
//!
//! # Zweck
//! Der Warden verlässt sich nicht allein auf die Autorisierung des
//! Escalators. Er setzt eigene Regeln durch, die nur aus seiner eigenen,
//! root-eigenen Konfiguration (`/etc/harw-dod/warden.toml`, siehe
//! `config.rs`) stammen:
//!
//! - [`ActionAllowlist`]: welche Aktionsarten dieser Host überhaupt erlaubt
//!   (Vorgabe: nur Freeze und Thaw; Kill, Isolation und LiftIsolation nur,
//!   wenn der Betreiber sie ausdrücklich einträgt). `release-cgroup` ist
//!   stillgelegt und nie eintragbar.
//! - [`DenyList`]: cgroups, die nie Ziel eines Eingriffs sein dürfen. Die
//!   eingebauten Einträge ([`BUILTIN_DENY_PATTERNS`]) decken Warden,
//!   Escalator, Sentinel, Probes, `harw-dod.slice`, `system.slice`,
//!   `init.scope`, die Sitzungs-Scopes, den User-Manager samt seiner Slices
//!   (`app.slice`, `session.slice`, ...), die Dienste direkt unter dem
//!   User-Manager und die User-Unit `harw-dod-watch.service` ab; die
//!   Konfiguration kann nur **ergänzen**, nie entfernen.
//! - die [`ProofPolicy`] für die Prüfung der Autorisierung (TTL, Uhrenversatz,
//!   erlaubte cgroup-Präfixe).
//!
//! # Grammatik eines Deny-Musters
//! Ein Muster ist ein relativer cgroup-Pfad aus `/`-getrennten Komponenten.
//! Eine Komponente darf `*` enthalten (beliebige, auch leere Zeichenfolge
//! innerhalb **einer** Komponente). Ein abschließendes `/**` macht das
//! Muster zum Teilbaum-Muster. Ein Ziel ist verboten, wenn
//!
//! 1. es dem Muster **genau** entspricht,
//! 2. es ein **Vorfahre** einer cgroup ist, die dem Muster entsprechen
//!    könnte (ein Freeze oder Kill auf einen Elternknoten trifft alle Kinder:
//!    `user.slice` würde jede Sitzung einfrieren), oder
//! 3. das Muster ein Teilbaum-Muster ist und das Ziel darunter liegt.
//!
//! Ein ungültiger Zielpfad gilt immer als verboten (fail closed).
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Werttypen; `Send + Sync`.
//!
//! # Fehler
//! [`WardenError::ConfigInvalid`] für ungültige Muster oder Allowlists.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden::policy::DenyList;
//! use harw_types::CgroupId;
//!
//! let deny = DenyList::builtin();
//! let Ok(session) = CgroupId::try_from_str("user.slice/user-1000.slice/session-3.scope") else {
//!     return;
//! };
//! let Ok(job) = CgroupId::try_from_str("harw.slice/job-1") else { return };
//! assert!(deny.denies(&session));
//! assert!(!deny.denies(&job));
//! ```

use harw_dod_warden_proto::{ProofPolicy, WardenAction};
use harw_types::CgroupId;

use crate::error::WardenError;

/// Die Aktionsarten (Wire-Namen, [`WardenAction::kind_name`]), die eine
/// Allowlist enthalten darf. `release-cgroup` fehlt absichtlich (stillgelegt).
pub const ACCEPTED_ACTION_KINDS: [&str; 5] = [
    "freeze-cgroup",
    "thaw-cgroup",
    "isolate-network",
    "lift-isolation",
    "kill-process-tree",
];

/// Vorgabe der Allowlist, wenn `warden.toml` keine nennt: nur umkehrbares
/// Einfrieren und Auftauen.
pub const DEFAULT_ACTION_KINDS: [&str; 2] = ["freeze-cgroup", "thaw-cgroup"];

/// Eingebaute Deny-Muster; die Konfiguration kann sie nur ergänzen.
///
/// # Description
/// Die Einträge für die DoD-Dienste unter `system.slice` sind durch
/// `system.slice/**` bereits abgedeckt und stehen trotzdem einzeln da: sie
/// dokumentieren die Pflichtliste aus PL-100 WP 3.1 (Warden, Escalator,
/// Sentinel, Probes) und bleiben wirksam, falls ein Dienst per `Slice=` in
/// einen anderen Slice wandert, der nicht unter `system.slice` liegt
/// (dafür `harw-dod.slice/**`).
///
/// Der User-Manager (`user@*.service`) ist bewusst **kein**
/// Teilbaum-Muster: harw-Jobs eines Nutzers laufen als eigene Scopes
/// unterhalb des User-Managers (`.../user@1000.service/app.slice/harw-job-1.scope`)
/// und bleiben zulässige Ziele. Verboten sind dagegen
///
/// - der User-Manager selbst, sein `init.scope` und jeder Vorfahre;
/// - jeder Slice direkt unter dem User-Manager (`app.slice`,
///   `session.slice`, `background.slice`, ...) als Ganzes — ein Freeze oder
///   Kill auf `app.slice` träfe jede Anwendung der Sitzung;
/// - `session.slice` samt Inhalt (dbus-broker, pipewire und die anderen
///   Sitzungsdienste);
/// - jeder Dienst (`*.service`) **direkt** unter dem User-Manager samt
///   Inhalt: so liegen Units ohne `Slice=` bei älteren systemd-Fassungen
///   (vor `app.slice`) — darunter Sitzungsdienste und
///   `harw-dod-watch.service`;
/// - die O5-User-Unit `harw-dod-watch.service` direkt unter dem
///   User-Manager und in jedem seiner Slices.
///
/// Zulässig bleiben damit Job-Scopes (`*.scope`) unter `app.slice` und den
/// anderen Slices des User-Managers sowie Scopes direkt unter dem
/// User-Manager. Diese Einengung von `user@*.service` (kein Teilbaum-Verbot)
/// ist eine Auslegung der Pflichtliste aus PL-100 WP 3.1 und braucht die
/// Bestätigung des Plan-Eigentümers.
pub const BUILTIN_DENY_PATTERNS: [&str; 15] = [
    "init.scope/**",
    "system.slice/**",
    "harw-dod.slice/**",
    "system.slice/harw-warden.service/**",
    "system.slice/harw-escalator.service/**",
    "system.slice/harw-sentinel.service/**",
    "system.slice/harw-probe-*.service/**",
    "user.slice/user-*.slice/session-*.scope/**",
    "user.slice/user-*.slice/user@*.service",
    "user.slice/user-*.slice/user@*.service/init.scope/**",
    "user.slice/user-*.slice/user@*.service/*.slice",
    "user.slice/user-*.slice/user@*.service/session.slice/**",
    "user.slice/user-*.slice/user@*.service/*.service/**",
    "user.slice/user-*.slice/user@*.service/harw-dod-watch.service/**",
    "user.slice/user-*.slice/user@*.service/*.slice/harw-dod-watch.service/**",
];

// Obergrenze für zusätzliche Deny-Muster aus der Konfiguration.
const MAX_EXTRA_DENY_PATTERNS: usize = 256;

/// Menge der Aktionsarten, die dieser Warden ausführen darf.
///
/// # Description
/// Enthält nur Namen aus [`ACCEPTED_ACTION_KINDS`]. Paarregeln (geprüft in
/// [`Self::new`]): wer `freeze-cgroup` erlaubt, muss auch `thaw-cgroup`
/// erlauben, und wer `isolate-network` erlaubt, auch `lift-isolation` — ein
/// Eingriff, der sich auf diesem Host nicht wieder aufheben ließe, ist ein
/// Konfigurationsfehler.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::policy::ActionAllowlist;
///
/// assert!(ActionAllowlist::new(["freeze-cgroup", "thaw-cgroup"]).is_ok());
/// assert!(ActionAllowlist::new(["freeze-cgroup"]).is_err());
/// assert!(ActionAllowlist::new(["release-cgroup"]).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionAllowlist {
    kinds: Vec<&'static str>,
}

impl ActionAllowlist {
    /// Builds a validated allowlist from action kind names.
    ///
    /// # Arguments
    /// - `names` (`impl IntoIterator<Item = impl AsRef<str>>`): Wire-Namen
    ///   der Aktionsarten.
    ///
    /// # Errors
    /// - [`WardenError::ConfigInvalid`]: leer, unbekannter oder stillgelegter
    ///   Name, doppelter Name oder eine verletzte Paarregel.
    pub fn new<I, S>(names: I) -> Result<Self, WardenError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut kinds: Vec<&'static str> = Vec::new();
        for name in names {
            let Some(kind) = ACCEPTED_ACTION_KINDS
                .iter()
                .copied()
                .find(|accepted| *accepted == name.as_ref())
            else {
                return Err(WardenError::ConfigInvalid {
                    reason: "allowed_actions names an unknown or retired action kind",
                });
            };
            if kinds.contains(&kind) {
                return Err(WardenError::ConfigInvalid {
                    reason: "allowed_actions names an action kind twice",
                });
            }
            kinds.push(kind);
        }
        if kinds.is_empty() {
            return Err(WardenError::ConfigInvalid {
                reason: "allowed_actions must not be empty",
            });
        }
        if kinds.contains(&"freeze-cgroup") && !kinds.contains(&"thaw-cgroup") {
            return Err(WardenError::ConfigInvalid {
                reason: "allowing freeze-cgroup requires thaw-cgroup",
            });
        }
        if kinds.contains(&"isolate-network") && !kinds.contains(&"lift-isolation") {
            return Err(WardenError::ConfigInvalid {
                reason: "allowing isolate-network requires lift-isolation",
            });
        }
        kinds.sort_unstable();
        Ok(Self { kinds })
    }

    /// Returns the default allowlist (freeze and thaw only).
    #[must_use]
    pub fn freeze_and_thaw() -> Self {
        Self {
            kinds: DEFAULT_ACTION_KINDS.to_vec(),
        }
    }

    /// Returns whether `action` is allowed (never for a retired action).
    #[must_use]
    pub fn permits(&self, action: &WardenAction) -> bool {
        !action.is_retired() && self.kinds.contains(&action.kind_name())
    }

    /// Returns the allowed kind names, sorted.
    #[must_use]
    pub fn kinds(&self) -> &[&'static str] {
        &self.kinds
    }
}

/// Ein Deny-Muster (siehe Moduldoku, „Grammatik eines Deny-Musters“).
///
/// # Examples
/// ```rust
/// use harw_dod_warden::policy::DenyPattern;
///
/// assert!(DenyPattern::parse("system.slice/**").is_ok());
/// assert!(DenyPattern::parse("../etc").is_err());
/// assert!(DenyPattern::parse("a/**/b").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyPattern {
    components: Vec<String>,
    subtree: bool,
}

impl DenyPattern {
    /// Parses one deny pattern.
    ///
    /// # Arguments
    /// - `text` (`&str`): das Muster, z. B. `"user.slice/user-*.slice/session-*.scope/**"`.
    ///
    /// # Errors
    /// - [`WardenError::ConfigInvalid`]: leeres Muster, leere Komponente
    ///   (führender, doppelter oder abschließender `/`), `.`/`..`,
    ///   Steuerzeichen oder `**` an anderer Stelle als am Ende.
    pub fn parse(text: &str) -> Result<Self, WardenError> {
        let invalid = || WardenError::ConfigInvalid {
            reason: "deny pattern is not a well-formed relative cgroup pattern",
        };
        let (body, subtree) = match text.strip_suffix("/**") {
            Some(body) => (body, true),
            None => (text, false),
        };
        let mut components = Vec::new();
        for component in body.split('/') {
            let valid = !component.is_empty()
                && component != "."
                && component != ".."
                && !component.contains("**")
                && !component.chars().any(char::is_control);
            if !valid {
                return Err(invalid());
            }
            components.push(component.to_owned());
        }
        Ok(Self {
            components,
            subtree,
        })
    }

    /// Returns whether this pattern forbids a target with the given path components.
    ///
    /// # Description
    /// Siehe Moduldoku: genaue Entsprechung, Vorfahre einer möglichen
    /// Entsprechung oder (bei Teilbaum-Mustern) Nachfahre.
    #[must_use]
    pub fn denies_components(&self, target: &[&str]) -> bool {
        // `zip` endet mit der kürzeren Seite: verglichen wird der gemeinsame Präfix.
        let prefix_matches = target
            .iter()
            .zip(&self.components)
            .all(|(name, pattern)| glob_matches(pattern, name));
        if !prefix_matches {
            return false;
        }
        // Gleich lang (genaue Entsprechung) oder kürzer (Vorfahre) — verboten.
        if target.len() <= self.components.len() {
            return true;
        }
        // Länger: nur ein Teilbaum-Muster verbietet Nachfahren.
        self.subtree
    }

    /// Renders the pattern in its textual form.
    #[must_use]
    pub fn to_text(&self) -> String {
        let body = self.components.join("/");
        if self.subtree {
            format!("{body}/**")
        } else {
            body
        }
    }
}

/// Die Deny-Liste des Wardens: eingebaute plus konfigurierte Muster.
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyList {
    patterns: Vec<DenyPattern>,
}

impl DenyList {
    /// Returns the deny list with only the built-in patterns.
    #[must_use]
    pub fn builtin() -> Self {
        let patterns = BUILTIN_DENY_PATTERNS
            .iter()
            .filter_map(|text| DenyPattern::parse(text).ok())
            .collect();
        Self { patterns }
    }

    /// Returns the built-in patterns plus `extra` from the configuration.
    ///
    /// # Arguments
    /// - `extra` (`&[String]`): zusätzliche Muster aus `warden.toml`.
    ///
    /// # Errors
    /// - [`WardenError::ConfigInvalid`]: ein Muster ist ungültig oder es
    ///   sind mehr als 256.
    pub fn with_extra(extra: &[String]) -> Result<Self, WardenError> {
        if extra.len() > MAX_EXTRA_DENY_PATTERNS {
            return Err(WardenError::ConfigInvalid {
                reason: "too many deny patterns",
            });
        }
        let mut list = Self::builtin();
        for text in extra {
            let pattern = DenyPattern::parse(text)?;
            if !list.patterns.contains(&pattern) {
                list.patterns.push(pattern);
            }
        }
        Ok(list)
    }

    /// Returns whether `cgroup` must never be the target of an action.
    ///
    /// # Description
    /// Ein ungültiger Pfad (leere Komponente, `.`, `..`, Steuerzeichen) gilt
    /// immer als verboten.
    #[must_use]
    pub fn denies(&self, cgroup: &CgroupId) -> bool {
        let components: Vec<&str> = cgroup.as_str().split('/').collect();
        let well_formed = components.iter().all(|component| {
            !component.is_empty()
                && *component != "."
                && *component != ".."
                && !component.chars().any(char::is_control)
        });
        if !well_formed {
            return true;
        }
        self.patterns
            .iter()
            .any(|pattern| pattern.denies_components(&components))
    }

    /// Returns the textual patterns, built-in ones first.
    #[must_use]
    pub fn patterns(&self) -> Vec<String> {
        self.patterns.iter().map(DenyPattern::to_text).collect()
    }
}

/// Warum die Warden-eigene Policy eine Aktion ablehnt.
///
/// # Description
/// Reine Entscheidung, kein Fehler: beide Fälle gehen als
/// [`harw_dod_warden_proto::Denial::PolicyDenied`] über die Leitung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyViolation {
    /// Die Aktionsart steht nicht in der [`ActionAllowlist`].
    ActionNotAllowed,
    /// Das Ziel steht auf der [`DenyList`].
    TargetDenied,
}

/// Die vollständige Warden-eigene Policy.
///
/// # Examples
/// ```rust
/// use harw_dod_warden::policy::{ActionAllowlist, DenyList, PolicyViolation, WardenPolicy};
/// use harw_dod_warden_proto::{ProofPolicy, WardenAction};
/// use harw_types::CgroupId;
///
/// let Ok(proof) = ProofPolicy::new(60, 5, vec!["harw.slice".to_owned()]) else { return };
/// let policy = WardenPolicy::new(proof, ActionAllowlist::freeze_and_thaw(), DenyList::builtin());
/// let Ok(cgroup) = CgroupId::try_from_str("harw.slice/job-1") else { return };
/// let kill = WardenAction::KillProcessTree { cgroup };
/// assert_eq!(policy.violation(&kill), Some(PolicyViolation::ActionNotAllowed));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WardenPolicy {
    proof: ProofPolicy,
    actions: ActionAllowlist,
    deny: DenyList,
}

impl WardenPolicy {
    /// Bundles the three parts of the warden policy.
    #[must_use]
    pub fn new(proof: ProofPolicy, actions: ActionAllowlist, deny: DenyList) -> Self {
        Self {
            proof,
            actions,
            deny,
        }
    }

    /// Returns the proof verification policy.
    #[must_use]
    pub fn proof(&self) -> &ProofPolicy {
        &self.proof
    }

    /// Returns the action allowlist.
    #[must_use]
    pub fn actions(&self) -> &ActionAllowlist {
        &self.actions
    }

    /// Returns the deny list.
    #[must_use]
    pub fn deny(&self) -> &DenyList {
        &self.deny
    }

    /// Returns why `action` is not allowed on this host, or `None`.
    ///
    /// # Description
    /// Prüft zuerst die Aktionsart, dann das Ziel.
    #[must_use]
    pub fn violation(&self, action: &WardenAction) -> Option<PolicyViolation> {
        if !self.actions.permits(action) {
            return Some(PolicyViolation::ActionNotAllowed);
        }
        if self.deny.denies(action.cgroup()) {
            return Some(PolicyViolation::TargetDenied);
        }
        None
    }
}

// Glob mit `*` (beliebige, auch leere Zeichenfolge) innerhalb einer Komponente.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    // `split` liefert immer mindestens ein Teilstück.
    let Some(first) = parts.next() else {
        return name.is_empty();
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    let Some((last, middle)) = tail.split_last() else {
        // Kein `*` im Muster: genaue Gleichheit.
        return rest.is_empty();
    };
    for part in middle {
        let Some(position) = rest.find(part) else {
            return false;
        };
        let Some(end) = position.checked_add(part.len()) else {
            return false;
        };
        let Some((_, after)) = rest.split_at_checked(end) else {
            return false;
        };
        rest = after;
    }
    rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::{
        ACCEPTED_ACTION_KINDS, ActionAllowlist, BUILTIN_DENY_PATTERNS, DenyList, DenyPattern,
        PolicyViolation, WardenPolicy, glob_matches,
    };
    use crate::error::WardenError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden_proto::{ProofPolicy, WardenAction};
    use harw_types::CgroupId;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    #[test]
    fn test_glob_matches_within_one_component() {
        assert!(glob_matches("session-*.scope", "session-3.scope"));
        assert!(glob_matches("user@*.service", "user@1000.service"));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("a*b*c", "abc"));
        assert!(glob_matches("a*b*c", "a-x-b-y-c"));
        assert!(!glob_matches("a*b*c", "ac"));
        assert!(!glob_matches("session-*.scope", "session-3.service"));
        assert!(!glob_matches("init.scope", "init.scope2"));
        assert!(glob_matches("init.scope", "init.scope"));
    }

    #[test]
    fn test_builtin_patterns_all_parse() -> TestResult {
        for text in BUILTIN_DENY_PATTERNS {
            let pattern = DenyPattern::parse(text).map_err(ctx("builtin pattern parses"))?;
            assert_eq!(pattern.to_text(), text);
        }
        assert_eq!(
            DenyList::builtin().patterns().len(),
            BUILTIN_DENY_PATTERNS.len()
        );
        Ok(())
    }

    #[test]
    fn test_builtin_deny_list_covers_the_mandatory_targets() -> TestResult {
        let deny = DenyList::builtin();
        for denied in [
            "init.scope",
            "system.slice",
            "system.slice/sshd.service",
            "system.slice/harw-warden.service",
            "system.slice/harw-escalator.service",
            "system.slice/harw-sentinel.service",
            "system.slice/harw-probe-bpf.service",
            "harw-dod.slice",
            "harw-dod.slice/harw-sentinel.service",
            "user.slice/user-1000.slice/session-3.scope",
            "user.slice/user-1000.slice/session-3.scope/child",
            "user.slice/user-1000.slice/user@1000.service",
            "user.slice/user-1000.slice/user@1000.service/init.scope",
            // Slices des User-Managers als Ganzes und die Sitzungsdienste.
            "user.slice/user-1000.slice/user@1000.service/app.slice",
            "user.slice/user-1000.slice/user@1000.service/session.slice",
            "user.slice/user-1000.slice/user@1000.service/background.slice",
            "user.slice/user-1000.slice/user@1000.service/session.slice/dbus-broker.service",
            "user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service",
            // Die O5-User-Unit, in welchem Slice des User-Managers auch immer.
            "user.slice/user-1000.slice/user@1000.service/app.slice/harw-dod-watch.service",
            "user.slice/user-1000.slice/user@1000.service/app.slice/harw-dod-watch.service/sub",
            "user.slice/user-1000.slice/user@1000.service/background.slice/harw-dod-watch.service",
            // Units direkt unter dem User-Manager (ältere systemd-Fassungen,
            // Units ohne `Slice=`), auch die O5-User-Unit.
            "user.slice/user-1000.slice/user@1000.service/harw-dod-watch.service",
            "user.slice/user-1000.slice/user@1000.service/harw-dod-watch.service/sub",
            "user.slice/user-1000.slice/user@1000.service/dbus.service",
            "user.slice/user-1000.slice/user@1000.service/pipewire.service",
            // Vorfahren geschützter cgroups: ein Freeze träfe alle Kinder.
            "user.slice",
            "user.slice/user-1000.slice",
        ] {
            if !deny.denies(&cgroup(denied)?) {
                return Err(TestError::Unexpected(format!("{denied} must be denied")));
            }
        }
        for allowed in [
            "harw.slice/job-1",
            "harw.slice",
            // Job-Scopes unter dem User-Manager bleiben zulässige Ziele.
            "user.slice/user-1000.slice/user@1000.service/app.slice/harw-job-1.scope",
            "user.slice/user-1000.slice/user@1000.service/app.slice/harw-job-1.scope/child",
            "user.slice/user-1000.slice/user@1000.service/background.slice/harw-job-3.scope",
            "user.slice/user-1000.slice/user@1000.service/harw-job-4.scope",
            "user.slice/user-1000.slice/harw-job-2.scope",
            "machine.slice/container-1.scope",
        ] {
            if deny.denies(&cgroup(allowed)?) {
                return Err(TestError::Unexpected(format!("{allowed} must be allowed")));
            }
        }
        Ok(())
    }

    #[test]
    fn test_malformed_target_is_always_denied() -> TestResult {
        let deny = DenyList::builtin();
        for malformed in ["harw.slice/../system.slice", "/harw.slice", "a//b", "a/./b"] {
            if !deny.denies(&cgroup(malformed)?) {
                return Err(TestError::Unexpected(format!(
                    "{malformed:?} must be denied"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_extra_patterns_add_but_never_remove() -> TestResult {
        let deny = DenyList::with_extra(&["harw.slice/protected-*.scope/**".to_owned()])
            .map_err(ctx("extra pattern parses"))?;
        assert!(deny.denies(&cgroup("harw.slice/protected-1.scope")?));
        assert!(deny.denies(&cgroup("harw.slice/protected-1.scope/sub")?));
        assert!(deny.denies(&cgroup("system.slice")?));
        assert!(!deny.denies(&cgroup("harw.slice/job-1")?));
        assert_eq!(
            deny.patterns().len(),
            BUILTIN_DENY_PATTERNS.len().saturating_add(1)
        );
        Ok(())
    }

    #[test]
    fn test_invalid_deny_patterns_are_rejected() {
        for bad in [
            "", "/abs", "a//b", "a/", "..", "a/../b", "a/**/b", "**", "a/b\u{7}",
        ] {
            assert!(
                matches!(
                    DenyPattern::parse(bad),
                    Err(WardenError::ConfigInvalid { .. })
                ),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn test_exact_pattern_does_not_deny_descendants() -> TestResult {
        let pattern = DenyPattern::parse("user.slice/user-*.slice/user@*.service")
            .map_err(ctx("pattern parses"))?;
        assert!(pattern.denies_components(&["user.slice", "user-1.slice", "user@1.service"]));
        assert!(pattern.denies_components(&["user.slice"]));
        assert!(!pattern.denies_components(&[
            "user.slice",
            "user-1.slice",
            "user@1.service",
            "app.slice"
        ]));
        assert!(!pattern.denies_components(&["harw.slice"]));
        Ok(())
    }

    #[test]
    fn test_allowlist_rules() {
        assert!(ActionAllowlist::new(ACCEPTED_ACTION_KINDS).is_ok());
        assert!(ActionAllowlist::new(["thaw-cgroup"]).is_ok());
        assert!(ActionAllowlist::new(["freeze-cgroup"]).is_err());
        assert!(ActionAllowlist::new(["isolate-network", "freeze-cgroup", "thaw-cgroup"]).is_err());
        assert!(ActionAllowlist::new(["release-cgroup", "thaw-cgroup"]).is_err());
        assert!(ActionAllowlist::new(["thaw-cgroup", "thaw-cgroup"]).is_err());
        assert!(ActionAllowlist::new(Vec::<String>::new()).is_err());
    }

    #[test]
    fn test_policy_violation_checks_kind_then_target() -> TestResult {
        let proof =
            ProofPolicy::new(60, 5, vec!["harw.slice".to_owned()]).map_err(ctx("proof policy"))?;
        let policy = WardenPolicy::new(
            proof,
            ActionAllowlist::freeze_and_thaw(),
            DenyList::builtin(),
        );
        let freeze_job = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let freeze_system = WardenAction::FreezeCgroup {
            cgroup: cgroup("system.slice/sshd.service")?,
        };
        let kill_job = WardenAction::KillProcessTree {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        assert_eq!(policy.violation(&freeze_job), None);
        assert_eq!(
            policy.violation(&freeze_system),
            Some(PolicyViolation::TargetDenied)
        );
        assert_eq!(
            policy.violation(&kill_job),
            Some(PolicyViolation::ActionNotAllowed)
        );
        Ok(())
    }
}
