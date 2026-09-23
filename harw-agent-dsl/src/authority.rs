//! Authority-Ceiling und monotone Reduzierung (§7, §12, §22 Invariante 7 DSL-Spec).
//!
//! Dieses Modul implementiert [`AuthorityCeiling`], die Darstellung einer
//! Capabilities-Menge, die entlang der Definitions-Erbschaftskette nur
//! monoton reduziert werden darf.
//!
//! # Schlüsseltypen
//! - [`AuthorityCeiling`] — sortierte, deduplizierte Capabilities-Menge
//!
//! # Invarianten (§7, §22 Invariante 7)
//! - Authority-tragende Felder dürfen nur durch `intersect` und Entfernung verändert werden.
//! - Jede Kinddefition muss `child.capabilities ⊆ parent.capabilities` erfüllen.
//! - Patches, die neue Capabilities hinzufügen, werden durch den Resolver abgelehnt.
//!
//! # Nebenläufigkeit
//! `AuthorityCeiling` ist `Send + Sync` und klonierbar.

use serde::{Deserialize, Serialize};

/// Sortierte Capabilities-Menge einer Agentendefinition (§7, §12).
///
/// # Beschreibung
/// Stellt die Menge der erlaubten Capabilities dar. Diese Menge darf entlang
/// der Definitions-Erbschaftskette nur monoton reduziert werden (nie erhöht).
/// Die interne Repräsentation ist stets sortiert und dedupliziert.
///
/// # Nebenläufigkeit
/// `Send + Sync`; kann sicher zwischen Threads geteilt werden (unveränderlich nach Erstellung).
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::authority::AuthorityCeiling;
///
/// let parent = AuthorityCeiling {
///     capabilities: vec!["filesystem.read".to_owned(), "filesystem.write".to_owned()],
/// };
/// let child = AuthorityCeiling {
///     capabilities: vec!["filesystem.read".to_owned()],
/// };
/// assert!(child.is_reduction_of(&parent));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AuthorityCeiling {
    /// Sortierte, deduplizierte Capabilities-Menge als Strings.
    pub capabilities: Vec<String>,
}

impl AuthorityCeiling {
    /// Prüft, ob `self` eine echte Teilmenge oder gleich `parent` ist (§7).
    ///
    /// # Beschreibung
    /// Gibt `true` zurück, wenn jede Capability in `self` auch in `parent`
    /// vorhanden ist (`self.capabilities ⊆ parent.capabilities`).
    ///
    /// # Argumente
    /// - `parent` (`&Self`): die Elterndefinition, gegen die geprüft wird.
    ///
    /// # Rückgabe
    /// `true` wenn `self ⊆ parent`, `false` wenn `self` neue Capabilities enthält.
    ///
    /// # Nebenläufigkeit
    /// Zustandslos; thread-sicher.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_agent_dsl::authority::AuthorityCeiling;
    ///
    /// let parent = AuthorityCeiling { capabilities: vec!["a".to_owned(), "b".to_owned()] };
    /// let child = AuthorityCeiling { capabilities: vec!["a".to_owned()] };
    /// assert!(child.is_reduction_of(&parent));
    /// ```
    pub fn is_reduction_of(&self, parent: &Self) -> bool {
        self.capabilities
            .iter()
            .all(|cap| parent.capabilities.contains(cap))
    }

    /// Kombiniert zwei Ceiling-Mengen per Schnittbildung (§7, nur `intersect` erlaubt für Authority).
    ///
    /// # Beschreibung
    /// Gibt eine neue `AuthorityCeiling` zurück, die nur Capabilities enthält,
    /// die in **beiden** Mengen vorhanden sind. Das Ergebnis ist sortiert und dedupliziert.
    ///
    /// # Argumente
    /// - `other` (`&Self`): die zweite Capabilities-Menge.
    ///
    /// # Rückgabe
    /// Neue `AuthorityCeiling` mit dem Schnitt beider Mengen.
    ///
    /// # Nebenläufigkeit
    /// Zustandslos; thread-sicher.
    ///
    /// # Beispiele
    /// ```rust
    /// use harw_agent_dsl::authority::AuthorityCeiling;
    ///
    /// let a = AuthorityCeiling { capabilities: vec!["r".to_owned(), "w".to_owned()] };
    /// let b = AuthorityCeiling { capabilities: vec!["r".to_owned(), "x".to_owned()] };
    /// let result = a.intersect(&b);
    /// assert_eq!(result.capabilities, vec!["r".to_owned()]);
    /// ```
    pub fn intersect(&self, other: &Self) -> Self {
        let mut result: Vec<String> = self
            .capabilities
            .iter()
            .filter(|cap| other.capabilities.contains(cap))
            .cloned()
            .collect();
        result.sort();
        result.dedup();
        AuthorityCeiling {
            capabilities: result,
        }
    }

    /// Prüft, ob `self` und `other` keine gemeinsame Capability tragen (Knoten AW6-01).
    ///
    /// # Description
    /// Erweiterung von [`AuthorityCeiling`] um ein benanntes Disjunktheits-Prädikat.
    /// Wird von der Security-Familie (`harw-registry-defaults/agents/families/security/security.toml`)
    /// benutzt, um ihr `universe` (die obere Schranke der Capabilities, die
    /// irgendein Mitglied dieser Familie tragen darf) gegen das `universe`
    /// anderer Familien zu prüfen. Zwei Familien mit überschneidendem `universe`
    /// eröffnen einen Pfad von angreiferkontrolliertem Eingabematerial in einer
    /// Familie zu produktiven Werkzeugen einer anderen — die Prüfung schneidet
    /// diesen Pfad an der Wurzel ab, statt ihn nur zu dokumentieren.
    ///
    /// Implementiert über [`AuthorityCeiling::intersect`], das bereits sortiert
    /// und dedupliziert — es wird keine neue Mengenoperation eingeführt, nur ein
    /// Name für die Bedingung "der Schnitt ist leer".
    ///
    /// # Arguments
    /// - `other` (`&Self`): die zweite Capabilities-Menge, gegen die geprüft wird.
    ///
    /// # Returns
    /// `true`, wenn `self` und `other` keine gemeinsame Capability enthalten
    /// (einschließlich des Falls, dass eine der beiden Mengen leer ist —
    /// eine leere Menge ist trivial disjunkt zu jeder anderen); `false`, wenn
    /// mindestens eine Capability in beiden Mengen vorkommt.
    ///
    /// # Errors
    /// Keine — reine, unfehlbare Prüfung.
    ///
    /// # Examples
    /// ```rust
    /// use harw_agent_dsl::authority::AuthorityCeiling;
    ///
    /// let security = AuthorityCeiling { capabilities: vec!["security.sensor.read".to_owned()] };
    /// let coding = AuthorityCeiling { capabilities: vec!["filesystem.write".to_owned()] };
    /// assert!(security.is_disjoint_from(&coding));
    ///
    /// let overlapping = AuthorityCeiling { capabilities: vec!["security.sensor.read".to_owned()] };
    /// assert!(!security.is_disjoint_from(&overlapping));
    /// ```
    pub fn is_disjoint_from(&self, other: &Self) -> bool {
        self.intersect(other).capabilities.is_empty()
    }

    /// Gibt die Capabilities zurück, die in `self` aber nicht in `parent` sind.
    ///
    /// # Beschreibung
    /// Hilfs-Methode für den Resolver: Liefert die Menge der neu hinzugefügten
    /// (und damit unzulässigen) Capabilities.
    ///
    /// # Argumente
    /// - `parent` (`&Self`): die Elterndefinition.
    ///
    /// # Rückgabe
    /// Vec mit den unzulässig neuen Capabilities (leer wenn `self ⊆ parent`).
    pub fn added_relative_to(&self, parent: &Self) -> Vec<String> {
        self.capabilities
            .iter()
            .filter(|cap| !parent.capabilities.contains(cap))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ceiling(caps: &[&str]) -> AuthorityCeiling {
        AuthorityCeiling {
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn test_is_reduction_of_true() {
        let parent = ceiling(&[
            "filesystem.read",
            "filesystem.write",
            "process.spawn.sandboxed",
        ]);
        // Nur Felder aus parent verwenden
        let child_valid = ceiling(&["filesystem.read"]);
        assert!(child_valid.is_reduction_of(&parent));
    }

    #[test]
    fn test_is_reduction_of_false_when_new_added() {
        let parent = ceiling(&["filesystem.read"]);
        let child = ceiling(&["filesystem.read", "agent.spawn.child-orchestrator"]);
        assert!(!child.is_reduction_of(&parent));
    }

    #[test]
    fn test_is_reduction_empty_child() {
        let parent = ceiling(&["a", "b"]);
        let child = ceiling(&[]);
        assert!(child.is_reduction_of(&parent));
    }

    #[test]
    fn test_is_reduction_equal_sets() {
        let parent = ceiling(&["a", "b"]);
        let child = ceiling(&["a", "b"]);
        assert!(child.is_reduction_of(&parent));
    }

    #[test]
    fn test_intersect() {
        let a = ceiling(&[
            "filesystem.read",
            "filesystem.write.scoped",
            "process.spawn.sandboxed",
        ]);
        let b = ceiling(&["filesystem.read", "filesystem.write.scoped"]);
        let result = a.intersect(&b);
        let expected: Vec<String> = vec![
            "filesystem.read".to_owned(),
            "filesystem.write.scoped".to_owned(),
        ];
        assert_eq!(result.capabilities, expected);
    }

    #[test]
    fn test_intersect_disjoint() {
        let a = ceiling(&["a", "b"]);
        let b = ceiling(&["c", "d"]);
        assert!(a.intersect(&b).capabilities.is_empty());
    }

    #[test]
    fn test_intersect_sorted_and_deduped() {
        let a = ceiling(&["z", "a", "m", "a"]); // Duplikat und unsortiert
        let b = ceiling(&["a", "m", "z"]);
        let result = a.intersect(&b);
        assert_eq!(result.capabilities, vec!["a", "m", "z"]);
    }

    #[test]
    fn test_added_relative_to_empty_when_subset() {
        let parent = ceiling(&["a", "b", "c"]);
        let child = ceiling(&["a", "b"]);
        assert!(child.added_relative_to(&parent).is_empty());
    }

    #[test]
    fn test_added_relative_to_returns_new_caps() {
        let parent = ceiling(&["a"]);
        let child = ceiling(&["a", "b", "c"]);
        let added = child.added_relative_to(&parent);
        assert!(added.contains(&"b".to_owned()));
        assert!(added.contains(&"c".to_owned()));
    }

    // ------------------------------------------------------------------
    // `is_disjoint_from` (Knoten AW6-01) — Disjunktheitsprädikat für Familien-Universen.
    // ------------------------------------------------------------------

    /// Zwei Mengen ohne gemeinsames Element sind disjunkt.
    #[test]
    fn test_is_disjoint_from_true_for_no_overlap() {
        let security = ceiling(&["security.sensor.read", "security.verdict.propose"]);
        let coding = ceiling(&["filesystem.write", "process.spawn.sandboxed"]);
        assert!(security.is_disjoint_from(&coding));
        // Symmetrisch: die Prüfung darf nicht von der Aufrufrichtung abhängen.
        assert!(coding.is_disjoint_from(&security));
    }

    /// Eine leere Menge ist trivial disjunkt zu jeder anderen Menge — das ist
    /// der Fall, in dem eine Familie (noch) kein `universe` deklariert (z. B.
    /// `family/coding`, `family/research` vor Knoten AW6-01).
    #[test]
    fn test_is_disjoint_from_true_when_one_side_empty() {
        let empty = ceiling(&[]);
        let security = ceiling(&["security.sensor.read"]);
        assert!(empty.is_disjoint_from(&security));
        assert!(security.is_disjoint_from(&empty));
    }

    /// Der rote Fall, absichtlich konstruiert (§ Auftrag AW6-01: "konstruiere
    /// absichtlich eine Überschneidung und sieh nach, dass er rot wird"): zwei
    /// Universen, die dieselbe Capability tragen, dürfen NICHT als disjunkt
    /// gelten. Würde `is_disjoint_from` fälschlich `true` liefern (z. B. durch
    /// eine vertauschte Negation oder durch `union` statt `intersect`), schlägt
    /// genau diese Assertion fehl — das ist der Beweis, dass der Check greift,
    /// nicht nur zufällig grün läuft, weil beide Testfälle oben leere oder
    /// nicht überlappende Mengen benutzen.
    #[test]
    fn test_is_disjoint_from_false_for_deliberately_constructed_overlap() {
        let security = ceiling(&["security.sensor.read", "security.verdict.propose"]);
        let attacker_shaped_other_family = ceiling(&["security.sensor.read", "filesystem.write"]);
        assert!(
            !security.is_disjoint_from(&attacker_shaped_other_family),
            "zwei Universen, die 'security.sensor.read' teilen, muessen als NICHT disjunkt gelten"
        );
    }

    /// Eine Menge ist niemals disjunkt zu sich selbst, außer sie ist leer.
    #[test]
    fn test_is_disjoint_from_false_for_identical_nonempty_sets() {
        let a = ceiling(&["security.sensor.read"]);
        let b = ceiling(&["security.sensor.read"]);
        assert!(!a.is_disjoint_from(&b));
    }
}
