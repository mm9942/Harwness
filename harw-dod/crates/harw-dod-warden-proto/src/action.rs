//! Die Aktionstaxonomie: [`ProposedAction`], [`WardenAction`],
//! [`Reversibility`].
//!
//! # Verantwortungsbereich
//! Diese Aktionen sind von Hand geschrieben, nicht über
//! `harw_macros::warden_actions!` erzeugt — siehe `lib.rs`-Moduldoku,
//! Abschnitt „Prüfung 2: harw-tools" für die Begründung. Beide Formen halten
//! sich an dieselbe Disziplin, die das Makro erzwingen würde: kein Feld
//! außerhalb der Positivliste aus `harw-types`-Kennungen, Ganzzahlen, `bool`
//! oder `Vec<T>` darüber (siehe `lib.rs`-Moduldoku, Abschnitt „Keine freien
//! Argumente").
//!
//! # Warum genau diese Aktionen (PL-100 WP 3.1)
//! Aus dem, was der DoD-Teilbaum durchsetzen können muss, je ein Eingriff und
//! seine eigene, getrennte Aufhebung:
//! - Einfrieren ([`WardenAction::FreezeCgroup`]) und Auftauen
//!   ([`WardenAction::ThawCgroup`]);
//! - Netzabschneiden ([`WardenAction::IsolateNetwork`]) und Aufheben der
//!   Isolation ([`WardenAction::LiftIsolation`]);
//! - Beenden ([`WardenAction::KillProcessTree`]), ohne Aufhebung.
//!
//! **Stillgelegt:** `ReleaseCgroup` hob Freeze und Isolation gemeinsam auf.
//! Weil Auftauen schon ab `RuleTriggered` zulässig sein soll, das Aufheben
//! einer Isolation aber erst ab `Escalated`, ließ sich eine gemeinsame
//! Aufhebung nicht mit einer Stufe binden. Die Variante bleibt nur als
//! `#[deprecated]`-Altlast für die v1-Aufrufer stehen
//! ([`WardenAction::is_retired`]); Proof v2 lehnt sie immer ab
//! ([`crate::error::ProofError::RetiredAction`]).
//!
//! # Die Zulässigkeitsmatrix in Worten
//! `FreezeCgroup` und `ThawCgroup` sind bereits ab `RuleTriggered`
//! zulässig: Einfrieren ist billig und reversibel, und Auftauen darf nie
//! restriktiver sein als das Einfrieren, das es aufhebt — sonst gäbe es
//! Zustände, aus denen sich ein Freeze, dessen Anlass inzwischen abgeklungen
//! ist, nicht mehr zurücknehmen lässt. `IsolateNetwork`, `LiftIsolation` und
//! `KillProcessTree` sind erst ab `Escalated` zulässig: Netzabschnitt und
//! Beenden sind teurer (Netzabschnitt bricht laufende, möglicherweise
//! legitime Verbindungen; ein Prozessbaum, einmal beendet, ist
//! unwiederbringlich), und eine Isolation wird nur auf derselben Stufe
//! aufgehoben, auf der sie verhängt werden durfte.
//!
//! # Reversibilität und Bestätigung „nicht umkehrbar“
//! Technisch reversibel sind `FreezeCgroup` (über `ThawCgroup`),
//! `ThawCgroup` (über erneutes Einfrieren), `IsolateNetwork` (über
//! `LiftIsolation`) und `LiftIsolation` (über erneutes Isolieren).
//! `KillProcessTree` ist ausdrücklich **nicht** reversibel.
//! [`WardenAction::reversibility`] macht das erschöpfend (compilergeprüft)
//! explizit.
//!
//! Unabhängig davon verlangt Entscheidung O4 für Kill **und** Isolation eine
//! ausdrückliche Bestätigung „nicht umkehrbar“ des Operators: eine
//! abgebrochene Verbindung lässt sich nicht wiederherstellen, auch wenn die
//! Isolation selbst aufhebbar ist. [`WardenAction::requires_irreversible_ack`]
//! benennt diese Menge; Proof v2 prüft das signierte `irreversible_ack`.

// Dieses Modul definiert die stillgelegten Varianten selbst und muss sie in
// seinen erschöpfenden `match`-Stellen nennen; die Warnung gilt weiter für
// jeden Aufrufer außerhalb dieses Moduls.
#![allow(deprecated)]

use harw_types::{CgroupId, ContentDigest};
use serde::{Deserialize, Serialize};

use crate::error::WardenProtoError;
use crate::stage::EscalationStage;

/// Was ein Agent vorschlagen kann, ohne Autorisierung.
///
/// # Description
/// Strukturell identisch zu [`WardenAction`], aber ein eigener Typ: Code,
/// das eine `ProposedAction` hält, kann sie nicht versehentlich
/// unautorisiert an den Durchsetzer senden — dafür ist erst die Umwandlung in
/// eine [`WardenAction`] und eine vom Escalator signierte
/// [`crate::signed::SignedAuthorization`] nötig (dieselbe Trennung, die
/// `harw_macros::warden_actions!` erzeugen würde, siehe dessen Moduldoku,
/// Abschnitt „Was pro Deklaration erzeugt wird").
///
/// # Wire-Format
/// Intern getaggt (`"kind"`), kebab-case, `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProposedAction {
    /// Friert eine cgroup ein. Reversibel über [`Self::ThawCgroup`].
    FreezeCgroup {
        /// Die einzufrierende cgroup.
        cgroup: CgroupId,
    },
    /// Taut eine eingefrorene cgroup wieder auf (hebt nur den Freeze auf,
    /// nie eine Netzisolation).
    ThawCgroup {
        /// Die aufzutauende cgroup.
        cgroup: CgroupId,
    },
    /// **Stillgelegt** (v1): hob Freeze und Netzisolation gemeinsam auf.
    /// Ersetzt durch [`Self::ThawCgroup`] und [`Self::LiftIsolation`].
    #[deprecated(
        note = "retired by PL-100 WP 3.1: use ThawCgroup to thaw and LiftIsolation to lift a network isolation; warden proof v2 refuses ReleaseCgroup"
    )]
    ReleaseCgroup {
        /// Die freizugebende cgroup.
        cgroup: CgroupId,
    },
    /// Schneidet den Netzzugriff einer cgroup ab. Reversibel über
    /// [`Self::LiftIsolation`]; verlangt trotzdem `irreversible_ack` (O4).
    IsolateNetwork {
        /// Die zu isolierende cgroup.
        cgroup: CgroupId,
    },
    /// Hebt die Netzisolation einer cgroup auf (nie einen Freeze).
    LiftIsolation {
        /// Die cgroup, deren Isolation aufgehoben wird.
        cgroup: CgroupId,
    },
    /// Beendet den Prozessbaum einer cgroup. **Nicht reversibel.**
    KillProcessTree {
        /// Die cgroup, deren Prozessbaum beendet wird.
        cgroup: CgroupId,
    },
}

/// Wire-Enum: die Aktionstaxonomie, wie sie (eingebettet in
/// [`crate::response::WardenRequest`]) über den Socket zum Durchsetzer geht.
///
/// # Description
/// Datenform identisch zu [`ProposedAction`] — siehe dort für die
/// Begründung der Trennung. `From<ProposedAction>` ist die einzige
/// Umwandlung; sie verändert keine Feldwerte.
///
/// # Wire-Format
/// Intern getaggt (`"kind"`), kebab-case, `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WardenAction {
    /// Siehe [`ProposedAction::FreezeCgroup`].
    FreezeCgroup {
        /// Die einzufrierende cgroup.
        cgroup: CgroupId,
    },
    /// Siehe [`ProposedAction::ThawCgroup`].
    ThawCgroup {
        /// Die aufzutauende cgroup.
        cgroup: CgroupId,
    },
    /// **Stillgelegt** (v1), siehe [`ProposedAction::ReleaseCgroup`]. Proof v2
    /// lehnt diese Variante immer ab ([`Self::is_retired`]).
    #[deprecated(
        note = "retired by PL-100 WP 3.1: use ThawCgroup to thaw and LiftIsolation to lift a network isolation; warden proof v2 refuses ReleaseCgroup"
    )]
    ReleaseCgroup {
        /// Die freizugebende cgroup.
        cgroup: CgroupId,
    },
    /// Siehe [`ProposedAction::IsolateNetwork`].
    IsolateNetwork {
        /// Die zu isolierende cgroup.
        cgroup: CgroupId,
    },
    /// Siehe [`ProposedAction::LiftIsolation`].
    LiftIsolation {
        /// Die cgroup, deren Isolation aufgehoben wird.
        cgroup: CgroupId,
    },
    /// Siehe [`ProposedAction::KillProcessTree`].
    KillProcessTree {
        /// Die cgroup, deren Prozessbaum beendet wird.
        cgroup: CgroupId,
    },
}

/// Überträgt eine vorgeschlagene Aktion unverändert in die Wire-Form.
///
/// # Description
/// Reine Struktur-zu-Struktur-Übertragung; kein Feldwert ändert sich (siehe
/// Testfall `test_from_proposed_action_preserves_all_fields`).
impl From<ProposedAction> for WardenAction {
    fn from(value: ProposedAction) -> Self {
        match value {
            ProposedAction::FreezeCgroup { cgroup } => Self::FreezeCgroup { cgroup },
            ProposedAction::ThawCgroup { cgroup } => Self::ThawCgroup { cgroup },
            ProposedAction::ReleaseCgroup { cgroup } => Self::ReleaseCgroup { cgroup },
            ProposedAction::IsolateNetwork { cgroup } => Self::IsolateNetwork { cgroup },
            ProposedAction::LiftIsolation { cgroup } => Self::LiftIsolation { cgroup },
            ProposedAction::KillProcessTree { cgroup } => Self::KillProcessTree { cgroup },
        }
    }
}

/// Ob eine [`WardenAction`] rückgängig gemacht werden kann.
///
/// # Description
/// Siehe `action.rs`-Moduldoku, Abschnitt „Reversibilität", für die
/// Begründung je Aktion. Kein `Unknown`/`Unspecified`-Zweig: jede Aktion
/// muss sich für genau eine der beiden Varianten entscheiden, sobald sie
/// deklariert wird (erzwungen durch die erschöpfende `match`-Stelle in
/// [`WardenAction::reversibility`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reversibility {
    /// Eine andere (oder dieselbe, erneut angewendete) Aktion macht die
    /// Wirkung rückgängig.
    Reversible,
    /// Keine Aktion macht die Wirkung rückgängig.
    Irreversible,
}

impl WardenAction {
    /// Der deklarierte Audit-Name dieser Aktion.
    ///
    /// # Description
    /// Fester, zur Compile-Zeit gewählter Bezeichner je Aktion — kein vom
    /// Modell beeinflusster Wert (siehe `lib.rs`-Moduldoku, Abschnitt „Keine
    /// freien Argumente"). Verwendet von
    /// [`crate::response::WardenActionAudit::for_action`].
    ///
    /// # Returns
    /// Einen statischen, punktgetrennten Bezeichner (`"warden.<aktion>"`).
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::WardenAction;
    /// use harw_types::CgroupId;
    ///
    /// let action = WardenAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// assert_eq!(action.audit_name(), "warden.freeze_cgroup");
    /// ```
    #[must_use]
    pub fn audit_name(&self) -> &'static str {
        match self {
            Self::FreezeCgroup { .. } => "warden.freeze_cgroup",
            Self::ThawCgroup { .. } => "warden.thaw_cgroup",
            Self::ReleaseCgroup { .. } => "warden.release_cgroup",
            Self::IsolateNetwork { .. } => "warden.isolate_network",
            Self::LiftIsolation { .. } => "warden.lift_isolation",
            Self::KillProcessTree { .. } => "warden.kill_process_tree",
        }
    }

    /// Befragt die Zulässigkeitsmatrix: darf diese Aktion ab `stage`
    /// durchgesetzt werden?
    ///
    /// # Description
    /// Siehe `action.rs`-Moduldoku, Abschnitt „Die Zulässigkeitsmatrix in
    /// Worten", für die Begründung je Aktion. Die stillgelegte Variante
    /// `ReleaseCgroup` behält ihre v1-Zeile (ab `RuleTriggered`), damit
    /// v1-Aufrufer unverändert weiterlaufen; Proof v2 lehnt sie unabhängig
    /// davon ab ([`Self::is_retired`]).
    ///
    /// # Arguments
    /// - `stage` (`EscalationStage`): die im Beleg genannte Stufe.
    ///
    /// # Returns
    /// `true`, wenn diese Aktion ab `stage` zulässig ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::{EscalationStage, WardenAction};
    /// use harw_types::CgroupId;
    ///
    /// let freeze = WardenAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// assert!(freeze.is_admissible_from(EscalationStage::RuleTriggered));
    ///
    /// let kill = WardenAction::KillProcessTree {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// assert!(!kill.is_admissible_from(EscalationStage::RuleTriggered));
    /// assert!(kill.is_admissible_from(EscalationStage::Escalated));
    /// ```
    #[must_use]
    pub fn is_admissible_from(&self, stage: EscalationStage) -> bool {
        match self {
            Self::FreezeCgroup { .. } | Self::ThawCgroup { .. } | Self::ReleaseCgroup { .. } => {
                matches!(
                    stage,
                    EscalationStage::RuleTriggered | EscalationStage::Escalated
                )
            }
            Self::IsolateNetwork { .. }
            | Self::LiftIsolation { .. }
            | Self::KillProcessTree { .. } => matches!(stage, EscalationStage::Escalated),
        }
    }

    /// Ob diese Aktion rückgängig gemacht werden kann.
    ///
    /// # Description
    /// Siehe `action.rs`-Moduldoku, Abschnitt „Reversibilität". Sagt nichts
    /// darüber, ob der Operator „nicht umkehrbar“ bestätigen muss — dafür
    /// [`Self::requires_irreversible_ack`].
    ///
    /// # Returns
    /// [`Reversibility::Reversible`] oder [`Reversibility::Irreversible`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::{Reversibility, WardenAction};
    /// use harw_types::CgroupId;
    ///
    /// let kill = WardenAction::KillProcessTree {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// assert_eq!(kill.reversibility(), Reversibility::Irreversible);
    /// ```
    #[must_use]
    pub fn reversibility(&self) -> Reversibility {
        match self {
            Self::FreezeCgroup { .. }
            | Self::ThawCgroup { .. }
            | Self::ReleaseCgroup { .. }
            | Self::IsolateNetwork { .. }
            | Self::LiftIsolation { .. } => Reversibility::Reversible,
            Self::KillProcessTree { .. } => Reversibility::Irreversible,
        }
    }

    /// Ob diese Aktion die signierte Bestätigung „nicht umkehrbar“ verlangt
    /// (`irreversible_ack`, Entscheidung O4).
    ///
    /// # Description
    /// `true` für `KillProcessTree` und `IsolateNetwork` (siehe Moduldoku,
    /// Abschnitt „Reversibilität und Bestätigung“). Erschöpfend: jede neue
    /// Variante muss sich hier entscheiden.
    ///
    /// # Returns
    /// `true`, wenn [`crate::signed::SignedAuthorization::verify`] ein
    /// signiertes `irreversible_ack == true` verlangt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::WardenAction;
    /// use harw_types::CgroupId;
    ///
    /// let Ok(cgroup) = CgroupId::try_from_str("harw.slice/job-1") else { return };
    /// assert!(WardenAction::IsolateNetwork { cgroup: cgroup.clone() }.requires_irreversible_ack());
    /// assert!(!WardenAction::FreezeCgroup { cgroup }.requires_irreversible_ack());
    /// ```
    #[must_use]
    pub const fn requires_irreversible_ack(&self) -> bool {
        match self {
            Self::IsolateNetwork { .. } | Self::KillProcessTree { .. } => true,
            Self::FreezeCgroup { .. }
            | Self::ThawCgroup { .. }
            | Self::ReleaseCgroup { .. }
            | Self::LiftIsolation { .. } => false,
        }
    }

    /// Ob diese Aktion stillgelegt ist und von Proof v2 nie akzeptiert wird.
    ///
    /// # Description
    /// Heute nur `ReleaseCgroup` (siehe Moduldoku, Abschnitt „Stillgelegt“).
    ///
    /// # Returns
    /// `true` für stillgelegte Varianten.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::WardenAction;
    /// use harw_types::CgroupId;
    ///
    /// let Ok(cgroup) = CgroupId::try_from_str("harw.slice/job-1") else { return };
    /// assert!(!WardenAction::ThawCgroup { cgroup }.is_retired());
    /// ```
    #[must_use]
    pub const fn is_retired(&self) -> bool {
        match self {
            Self::ReleaseCgroup { .. } => true,
            Self::FreezeCgroup { .. }
            | Self::ThawCgroup { .. }
            | Self::IsolateNetwork { .. }
            | Self::LiftIsolation { .. }
            | Self::KillProcessTree { .. } => false,
        }
    }

    /// Der Inhaltsdigest dieser Aktion — die Grundlage für
    /// [`crate::proof::AuthorizationProof::bound_action`] (**v1-Altlast**).
    ///
    /// # Description
    /// Kodiert die Aktion kanonisch als JSON (Feldreihenfolge folgt der
    /// Deklaration, siehe `serde_json`s Verhalten für von `#[derive]`
    /// erzeugte `Serialize`-Impls) und hasht das Ergebnis mit
    /// [`ContentDigest::of`]. Zwei Aufrufe mit strukturell identischen
    /// Aktionen (gleiche Variante, gleiche Feldwerte) ergeben denselben
    /// Digest; jede Abweichung — auch nur in einem `CgroupId`-Feld — ergibt
    /// einen anderen. Proof v2 nutzt stattdessen [`Self::binding_digest`]
    /// (serde-unabhängig, unfehlbar).
    ///
    /// # Errors
    /// - [`WardenProtoError::ActionEncoding`]: die JSON-Kodierung ist
    ///   fehlgeschlagen. Für die hier definierten Feldtypen (ausschließlich
    ///   `harw-types`-Kennungen) praktisch unerreichbar — dieselbe
    ///   Begründung wie bei `harw_dod_signals::SignalsError::DigestEncoding`.
    #[deprecated(
        note = "v1 binding for the forgeable AuthorizationProof (F-001); proof v2 binds WardenAction::binding_digest inside SignedAuthorization"
    )]
    pub fn content_digest(&self) -> Result<ContentDigest, WardenProtoError> {
        let bytes = serde_json::to_vec(self)?;
        Ok(ContentDigest::of(&bytes))
    }

    /// Returns the wire name of this action's variant (`"freeze-cgroup"`, …).
    ///
    /// # Description
    /// Identisch mit dem serde-Tag `kind` (kebab-case). Fester, zur
    /// Compile-Zeit gewählter Wert; Grundlage der kanonischen Aktionsbindung
    /// in [`Self::binding_digest`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::WardenAction;
    /// use harw_types::CgroupId;
    ///
    /// let action = WardenAction::KillProcessTree {
    ///     cgroup: CgroupId::try_from_str("harw.slice/job-1").unwrap(),
    /// };
    /// assert_eq!(action.kind_name(), "kill-process-tree");
    /// ```
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::FreezeCgroup { .. } => "freeze-cgroup",
            Self::ThawCgroup { .. } => "thaw-cgroup",
            Self::ReleaseCgroup { .. } => "release-cgroup",
            Self::IsolateNetwork { .. } => "isolate-network",
            Self::LiftIsolation { .. } => "lift-isolation",
            Self::KillProcessTree { .. } => "kill-process-tree",
        }
    }

    /// Returns the target cgroup of this action.
    ///
    /// # Description
    /// Jede Variante trägt genau eine Ziel-cgroup; Proof v2 bindet sie
    /// zusätzlich als Teil des Ziel-Tripels (siehe
    /// [`crate::signed::SignedAuthorization::verify`], Schritt „cgroup“).
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::WardenAction;
    /// use harw_types::CgroupId;
    ///
    /// let cgroup = CgroupId::try_from_str("harw.slice/job-1").unwrap();
    /// let action = WardenAction::FreezeCgroup { cgroup: cgroup.clone() };
    /// assert_eq!(action.cgroup(), &cgroup);
    /// ```
    #[must_use]
    pub const fn cgroup(&self) -> &CgroupId {
        match self {
            Self::FreezeCgroup { cgroup }
            | Self::ThawCgroup { cgroup }
            | Self::ReleaseCgroup { cgroup }
            | Self::IsolateNetwork { cgroup }
            | Self::LiftIsolation { cgroup }
            | Self::KillProcessTree { cgroup } => cgroup,
        }
    }

    /// Computes the canonical, serde-independent binding digest used by Proof v2.
    ///
    /// # Description
    /// BLAKE3 (unkeyed, über [`ContentDigest::of`]) über
    /// [`crate::canonical::ACTION_DOMAIN`] gefolgt von den längenpräfixierten
    /// Feldern `kind_name` und `cgroup` (siehe `canonical.rs`). Anders als
    /// der v1-Digest `content_digest` (JSON-basiert) hängt dieser Wert nicht
    /// von einer Serialisierungsbibliothek ab und ist unfehlbar.
    ///
    /// # Returns
    /// Den Bindungsdigest; strukturell gleiche Aktionen ergeben denselben Wert,
    /// jede Abweichung in Variante oder cgroup einen anderen.
    ///
    /// # Concurrency
    /// Reine Funktion.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::WardenAction;
    /// use harw_types::CgroupId;
    ///
    /// let Ok(cgroup) = CgroupId::try_from_str("harw.slice/job-1") else { return };
    /// let freeze = WardenAction::FreezeCgroup { cgroup: cgroup.clone() };
    /// let thaw = WardenAction::ThawCgroup { cgroup };
    /// assert_ne!(freeze.binding_digest(), thaw.binding_digest());
    /// ```
    #[must_use]
    pub fn binding_digest(&self) -> ContentDigest {
        let cgroup = self.cgroup().as_str().as_bytes();
        let kind = self.kind_name().as_bytes();
        let mut buf = Vec::with_capacity(
            crate::canonical::ACTION_DOMAIN
                .len()
                .saturating_add(16)
                .saturating_add(kind.len())
                .saturating_add(cgroup.len()),
        );
        buf.extend_from_slice(crate::canonical::ACTION_DOMAIN);
        crate::canonical::put_field(&mut buf, kind);
        crate::canonical::put_field(&mut buf, cgroup);
        ContentDigest::of(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::{ProposedAction, Reversibility, WardenAction};
    use crate::stage::EscalationStage;
    use crate::test_support::{TestResult, ctx};
    use harw_types::CgroupId;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    // Alle Varianten (auch die stillgelegte) für die erschöpfenden Tabellentests.
    fn every_action(target: &CgroupId) -> [WardenAction; 6] {
        [
            WardenAction::FreezeCgroup {
                cgroup: target.clone(),
            },
            WardenAction::ThawCgroup {
                cgroup: target.clone(),
            },
            WardenAction::ReleaseCgroup {
                cgroup: target.clone(),
            },
            WardenAction::IsolateNetwork {
                cgroup: target.clone(),
            },
            WardenAction::LiftIsolation {
                cgroup: target.clone(),
            },
            WardenAction::KillProcessTree {
                cgroup: target.clone(),
            },
        ]
    }

    // -- Wire-Rundlauf / deny_unknown_fields ---------------------------------

    #[test]
    fn test_proposed_action_serde_roundtrip() -> TestResult {
        let action = ProposedAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let json = serde_json::to_string(&action).map_err(ctx("serializes"))?;
        assert_eq!(json, r#"{"kind":"freeze-cgroup","cgroup":"cgroup-1"}"#);
        let round_tripped: ProposedAction =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, action);
        Ok(())
    }

    #[test]
    fn test_warden_action_serde_roundtrip() -> TestResult {
        let action = WardenAction::IsolateNetwork {
            cgroup: cgroup("cgroup-2")?,
        };
        let json = serde_json::to_string(&action).map_err(ctx("serializes"))?;
        let round_tripped: WardenAction =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, action);
        Ok(())
    }

    #[test]
    fn test_new_actions_use_kebab_case_wire_names() -> TestResult {
        let thaw = WardenAction::ThawCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        assert_eq!(
            serde_json::to_string(&thaw).map_err(ctx("serializes"))?,
            r#"{"kind":"thaw-cgroup","cgroup":"cgroup-1"}"#
        );
        let lift: WardenAction =
            serde_json::from_str(r#"{"kind":"lift-isolation","cgroup":"cgroup-1"}"#)
                .map_err(ctx("deserializes"))?;
        assert_eq!(
            lift,
            WardenAction::LiftIsolation {
                cgroup: cgroup("cgroup-1")?
            }
        );
        Ok(())
    }

    #[test]
    fn test_proposed_action_rejects_unknown_field() {
        let malformed = r#"{"kind":"freeze-cgroup","cgroup":"cgroup-1","extra":true}"#;
        let result: Result<ProposedAction, _> = serde_json::from_str(malformed);
        assert!(result.is_err());
    }

    #[test]
    fn test_warden_action_rejects_unknown_field() {
        let malformed = r#"{"kind":"kill-process-tree","cgroup":"cgroup-1","extra":true}"#;
        let result: Result<WardenAction, _> = serde_json::from_str(malformed);
        assert!(result.is_err());
    }

    // -- From<ProposedAction> ------------------------------------------------

    #[test]
    fn test_from_proposed_action_preserves_all_fields() -> TestResult {
        for proposed in [
            ProposedAction::FreezeCgroup {
                cgroup: cgroup("cgroup-a")?,
            },
            ProposedAction::ThawCgroup {
                cgroup: cgroup("cgroup-e")?,
            },
            ProposedAction::ReleaseCgroup {
                cgroup: cgroup("cgroup-b")?,
            },
            ProposedAction::IsolateNetwork {
                cgroup: cgroup("cgroup-c")?,
            },
            ProposedAction::LiftIsolation {
                cgroup: cgroup("cgroup-f")?,
            },
            ProposedAction::KillProcessTree {
                cgroup: cgroup("cgroup-d")?,
            },
        ] {
            let json_before = serde_json::to_string(&proposed).map_err(ctx("serializes"))?;
            let action: WardenAction = proposed.into();
            let json_after = serde_json::to_string(&action).map_err(ctx("serializes"))?;
            assert_eq!(json_before, json_after);
        }
        Ok(())
    }

    // -- audit_name ------------------------------------------------------------

    #[test]
    fn test_audit_name_matches_declared_convention_per_action() -> TestResult {
        let names: Vec<&str> = every_action(&cgroup("c")?)
            .iter()
            .map(WardenAction::audit_name)
            .collect();
        assert_eq!(
            names,
            [
                "warden.freeze_cgroup",
                "warden.thaw_cgroup",
                "warden.release_cgroup",
                "warden.isolate_network",
                "warden.lift_isolation",
                "warden.kill_process_tree",
            ]
        );
        Ok(())
    }

    // -- Zulässigkeitsmatrix -----------------------------------------------------

    #[test]
    fn test_freeze_and_thaw_admissible_from_rule_triggered_and_escalated() -> TestResult {
        for action in [
            WardenAction::FreezeCgroup {
                cgroup: cgroup("c")?,
            },
            WardenAction::ThawCgroup {
                cgroup: cgroup("c")?,
            },
        ] {
            assert!(action.is_admissible_from(EscalationStage::RuleTriggered));
            assert!(action.is_admissible_from(EscalationStage::Escalated));
        }
        Ok(())
    }

    #[test]
    fn test_isolate_lift_and_kill_admissible_only_from_escalated() -> TestResult {
        for action in [
            WardenAction::IsolateNetwork {
                cgroup: cgroup("c")?,
            },
            WardenAction::LiftIsolation {
                cgroup: cgroup("c")?,
            },
            WardenAction::KillProcessTree {
                cgroup: cgroup("c")?,
            },
        ] {
            assert!(!action.is_admissible_from(EscalationStage::RuleTriggered));
            assert!(action.is_admissible_from(EscalationStage::Escalated));
        }
        Ok(())
    }

    #[test]
    fn test_retired_release_keeps_v1_matrix_row() -> TestResult {
        let release = WardenAction::ReleaseCgroup {
            cgroup: cgroup("c")?,
        };
        assert!(release.is_admissible_from(EscalationStage::RuleTriggered));
        assert!(release.is_admissible_from(EscalationStage::Escalated));
        Ok(())
    }

    // -- Reversibilität / Bestätigung / Stilllegung --------------------------------

    #[test]
    fn test_reversibility_is_exhaustively_marked_per_action() -> TestResult {
        let marks: Vec<Reversibility> = every_action(&cgroup("c")?)
            .iter()
            .map(WardenAction::reversibility)
            .collect();
        assert_eq!(
            marks,
            [
                Reversibility::Reversible,
                Reversibility::Reversible,
                Reversibility::Reversible,
                Reversibility::Reversible,
                Reversibility::Reversible,
                Reversibility::Irreversible,
            ]
        );
        Ok(())
    }

    #[test]
    fn test_irreversible_ack_required_exactly_for_isolate_and_kill() -> TestResult {
        let required: Vec<bool> = every_action(&cgroup("c")?)
            .iter()
            .map(WardenAction::requires_irreversible_ack)
            .collect();
        assert_eq!(required, [false, false, false, true, false, true]);
        Ok(())
    }

    #[test]
    fn test_only_release_cgroup_is_retired() -> TestResult {
        let retired: Vec<bool> = every_action(&cgroup("c")?)
            .iter()
            .map(WardenAction::is_retired)
            .collect();
        assert_eq!(retired, [false, false, true, false, false, false]);
        Ok(())
    }

    // -- content_digest (v1) ----------------------------------------------------

    #[test]
    fn test_content_digest_is_deterministic_for_identical_actions() -> TestResult {
        let a = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let b = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        assert_eq!(
            a.content_digest().map_err(ctx("content_digest"))?,
            b.content_digest().map_err(ctx("content_digest"))?
        );
        Ok(())
    }

    #[test]
    fn test_content_digest_differs_for_different_cgroup() -> TestResult {
        let a = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let b = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-2")?,
        };
        assert_ne!(
            a.content_digest().map_err(ctx("content_digest"))?,
            b.content_digest().map_err(ctx("content_digest"))?
        );
        Ok(())
    }

    #[test]
    fn test_content_digest_differs_for_different_action_kind_with_same_cgroup() -> TestResult {
        let freeze = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let release = WardenAction::ReleaseCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        assert_ne!(
            freeze.content_digest().map_err(ctx("content_digest"))?,
            release.content_digest().map_err(ctx("content_digest"))?
        );
        Ok(())
    }

    // -- v2: kind_name / cgroup / binding_digest -----------------------------

    #[test]
    fn test_kind_name_matches_serde_tag_for_every_variant() -> TestResult {
        for action in every_action(&cgroup("c")?) {
            let value = serde_json::to_value(&action).map_err(ctx("serializes"))?;
            assert_eq!(
                value.get("kind").and_then(serde_json::Value::as_str),
                Some(action.kind_name())
            );
        }
        Ok(())
    }

    #[test]
    fn test_cgroup_returns_target_of_every_variant() -> TestResult {
        let target = cgroup("harw.slice/job-7")?;
        for action in every_action(&target) {
            assert_eq!(action.cgroup(), &target);
        }
        Ok(())
    }

    #[test]
    fn test_binding_digest_is_deterministic_and_field_sensitive() -> TestResult {
        let a = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let same = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let other_cgroup = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/job-2")?,
        };
        let other_kind = WardenAction::KillProcessTree {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        assert_eq!(a.binding_digest(), same.binding_digest());
        assert_ne!(a.binding_digest(), other_cgroup.binding_digest());
        assert_ne!(a.binding_digest(), other_kind.binding_digest());
        Ok(())
    }

    #[test]
    fn test_binding_digest_differs_for_every_pair_of_kinds() -> TestResult {
        let digests: Vec<_> = every_action(&cgroup("harw.slice/job-1")?)
            .iter()
            .map(WardenAction::binding_digest)
            .collect();
        for (i, left) in digests.iter().enumerate() {
            for right in digests.iter().skip(i.saturating_add(1)) {
                assert_ne!(left, right);
            }
        }
        Ok(())
    }
}
