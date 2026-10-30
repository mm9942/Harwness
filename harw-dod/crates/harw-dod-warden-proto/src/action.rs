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
//! # Warum genau diese vier Aktionen
//! Aus dem, was der DoD-Teilbaum durchsetzen können muss (Brief, Abschnitt
//! „Was zu bauen ist"): Einfrieren ([`WardenAction::FreezeCgroup`]),
//! Beenden ([`WardenAction::KillProcessTree`]), Netzabschneiden
//! ([`WardenAction::IsolateNetwork`]) und Freigeben
//! ([`WardenAction::ReleaseCgroup`], die gemeinsame Aufhebung für die beiden
//! erstgenannten — siehe [`Reversibility`]).
//!
//! # Die Zulässigkeitsmatrix in Worten
//! `FreezeCgroup` und `ReleaseCgroup` sind bereits ab `RuleTriggered`
//! zulässig: Einfrieren ist billig und reversibel, und eine Freigabe darf
//! nie restriktiver sein als die Maßnahme, die sie aufhebt — sonst gäbe es
//! Zustände, aus denen sich eine Eskalation, die inzwischen abgeklungen ist,
//! nicht mehr zurücknehmen lässt. `IsolateNetwork` und `KillProcessTree`
//! sind erst ab `Escalated` zulässig: beide sind teurer (Netzabschnitt
//! bricht laufende, möglicherweise legitime Verbindungen; ein Prozessbaum,
//! einmal beendet, ist unwiederbringlich), und ein bloßer Regeltreffer allein
//! rechtfertigt sie noch nicht.
//!
//! # Reversibilität
//! `FreezeCgroup`, `ReleaseCgroup` und `IsolateNetwork` sind reversibel —
//! `ReleaseCgroup` ist ihre gemeinsame Umkehrung, und `ReleaseCgroup` selbst
//! kehrt sich um, indem man erneut einfriert oder isoliert.
//! `KillProcessTree` ist ausdrücklich **nicht** reversibel: ein beendeter
//! Prozessbaum lässt sich nicht wiederherstellen. [`WardenAction::reversibility`]
//! macht das für jede Variante erschöpfend (compilergeprüft) explizit, statt
//! es nur hier in Prosa zu behaupten.

use harw_types::{CgroupId, ContentDigest};
use serde::{Deserialize, Serialize};

use crate::error::WardenProtoError;
use crate::stage::EscalationStage;

/// Was ein Agent vorschlagen kann, ohne Autorisierung.
///
/// # Description
/// Strukturell identisch zu [`WardenAction`], aber ein eigener Typ: Code,
/// das eine `ProposedAction` hält, kann sie nicht versehentlich
/// unautorisiert an den Durchsetzer senden — dafür ist immer erst
/// [`crate::response::WardenActionRequest::new`] mit einem
/// [`crate::proof::AuthorizationProof`] nötig (dieselbe Trennung, die
/// `harw_macros::warden_actions!` erzeugen würde, siehe dessen Moduldoku,
/// Abschnitt „Was pro Deklaration erzeugt wird").
///
/// # Wire-Format
/// Intern getaggt (`"kind"`), kebab-case, `deny_unknown_fields`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ProposedAction {
    /// Friert eine cgroup ein. Reversibel über [`Self::ReleaseCgroup`].
    FreezeCgroup {
        /// Die einzufrierende cgroup.
        cgroup: CgroupId,
    },
    /// Hebt eine bestehende Eindämmung (Freeze oder Netzisolation) für eine
    /// cgroup auf.
    ReleaseCgroup {
        /// Die freizugebende cgroup.
        cgroup: CgroupId,
    },
    /// Schneidet den Netzzugriff einer cgroup ab. Reversibel über
    /// [`Self::ReleaseCgroup`].
    IsolateNetwork {
        /// Die zu isolierende cgroup.
        cgroup: CgroupId,
    },
    /// Beendet den Prozessbaum einer cgroup. **Nicht reversibel.**
    KillProcessTree {
        /// Die cgroup, deren Prozessbaum beendet wird.
        cgroup: CgroupId,
    },
}

/// Wire-Enum: die Aktionstaxonomie, wie sie (eingebettet in
/// [`crate::response::WardenActionRequest`]) über den Socket zum Durchsetzer
/// geht.
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
    /// Siehe [`ProposedAction::ReleaseCgroup`].
    ReleaseCgroup {
        /// Die freizugebende cgroup.
        cgroup: CgroupId,
    },
    /// Siehe [`ProposedAction::IsolateNetwork`].
    IsolateNetwork {
        /// Die zu isolierende cgroup.
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
            ProposedAction::ReleaseCgroup { cgroup } => Self::ReleaseCgroup { cgroup },
            ProposedAction::IsolateNetwork { cgroup } => Self::IsolateNetwork { cgroup },
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
            Self::ReleaseCgroup { .. } => "warden.release_cgroup",
            Self::IsolateNetwork { .. } => "warden.isolate_network",
            Self::KillProcessTree { .. } => "warden.kill_process_tree",
        }
    }

    /// Befragt die Zulässigkeitsmatrix: darf diese Aktion ab `stage`
    /// durchgesetzt werden?
    ///
    /// # Description
    /// Siehe `action.rs`-Moduldoku, Abschnitt „Die Zulässigkeitsmatrix in
    /// Worten", für die Begründung je Aktion.
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
            Self::FreezeCgroup { .. } | Self::ReleaseCgroup { .. } => matches!(
                stage,
                EscalationStage::RuleTriggered | EscalationStage::Escalated
            ),
            Self::IsolateNetwork { .. } | Self::KillProcessTree { .. } => {
                matches!(stage, EscalationStage::Escalated)
            }
        }
    }

    /// Ob diese Aktion rückgängig gemacht werden kann.
    ///
    /// # Description
    /// Siehe `action.rs`-Moduldoku, Abschnitt „Reversibilität".
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
            | Self::ReleaseCgroup { .. }
            | Self::IsolateNetwork { .. } => Reversibility::Reversible,
            Self::KillProcessTree { .. } => Reversibility::Irreversible,
        }
    }

    /// Der Inhaltsdigest dieser Aktion — die Grundlage für
    /// [`crate::proof::AuthorizationProof::bound_action`].
    ///
    /// # Description
    /// Kodiert die Aktion kanonisch als JSON (Feldreihenfolge folgt der
    /// Deklaration, siehe `serde_json`s Verhalten für von `#[derive]`
    /// erzeugte `Serialize`-Impls) und hasht das Ergebnis mit
    /// [`ContentDigest::of`]. Zwei Aufrufe mit strukturell identischen
    /// Aktionen (gleiche Variante, gleiche Feldwerte) ergeben denselben
    /// Digest; jede Abweichung — auch nur in einem `CgroupId`-Feld — ergibt
    /// einen anderen. Das ist die Eigenschaft, die
    /// [`crate::proof::AuthorizationProof::authorizes`] braucht: ein Beleg,
    /// der auf eine bestimmte Aktion gebunden ist, passt auf keine andere.
    ///
    /// `ProposedAction` hat keine eigene `content_digest`-Methode: ihre
    /// Wire-Form ist byteidentisch zu der von `WardenAction` (dieselbe
    /// `#[serde(tag = "kind", ...)]`-Konfiguration, dieselben Varianten),
    /// also liefert `WardenAction::from(proposed).content_digest()` denselben
    /// Wert, den eine hypothetische `ProposedAction::content_digest()` auch
    /// ergeben hätte.
    ///
    /// # Errors
    /// - [`WardenProtoError::ActionEncoding`]: die JSON-Kodierung ist
    ///   fehlgeschlagen. Für die hier definierten Feldtypen (ausschließlich
    ///   `harw-types`-Kennungen) praktisch unerreichbar — dieselbe
    ///   Begründung wie bei `harw_dod_signals::SignalsError::DigestEncoding`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::WardenAction;
    /// use harw_types::CgroupId;
    ///
    /// let a = WardenAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// let b = WardenAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// assert_eq!(a.content_digest().unwrap(), b.content_digest().unwrap());
    /// ```
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
            Self::ReleaseCgroup { .. } => "release-cgroup",
            Self::IsolateNetwork { .. } => "isolate-network",
            Self::KillProcessTree { .. } => "kill-process-tree",
        }
    }

    /// Returns the target cgroup of this action.
    ///
    /// # Description
    /// Jede Variante trägt genau eine Ziel-cgroup; Proof v2 bindet sie
    /// zusätzlich separat (siehe [`crate::signed::SignedAuthorization::verify`],
    /// Schritt „cgroup-Präfix“).
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
            | Self::ReleaseCgroup { cgroup }
            | Self::IsolateNetwork { cgroup }
            | Self::KillProcessTree { cgroup } => cgroup,
        }
    }

    /// Computes the canonical, serde-independent binding digest used by Proof v2.
    ///
    /// # Description
    /// BLAKE3 (unkeyed, über [`ContentDigest::of`]) über
    /// [`crate::canonical::ACTION_DOMAIN`] gefolgt von den längenpräfixierten
    /// Feldern `kind_name` und `cgroup` (siehe `canonical.rs`). Anders als
    /// [`Self::content_digest`] (v1, JSON-basiert) hängt dieser Wert nicht
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
    /// let freeze = WardenAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("harw.slice/job-1").unwrap(),
    /// };
    /// let release = WardenAction::ReleaseCgroup {
    ///     cgroup: CgroupId::try_from_str("harw.slice/job-1").unwrap(),
    /// };
    /// assert_ne!(freeze.binding_digest(), release.binding_digest());
    /// ```
    #[must_use]
    pub fn binding_digest(&self) -> ContentDigest {
        let cgroup = self.cgroup().as_str().as_bytes();
        let kind = self.kind_name().as_bytes();
        let mut buf = Vec::with_capacity(
            crate::canonical::ACTION_DOMAIN.len() + 16 + kind.len() + cgroup.len(),
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
            ProposedAction::ReleaseCgroup {
                cgroup: cgroup("cgroup-b")?,
            },
            ProposedAction::IsolateNetwork {
                cgroup: cgroup("cgroup-c")?,
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
        assert_eq!(
            WardenAction::FreezeCgroup {
                cgroup: cgroup("c")?
            }
            .audit_name(),
            "warden.freeze_cgroup"
        );
        assert_eq!(
            WardenAction::ReleaseCgroup {
                cgroup: cgroup("c")?
            }
            .audit_name(),
            "warden.release_cgroup"
        );
        assert_eq!(
            WardenAction::IsolateNetwork {
                cgroup: cgroup("c")?
            }
            .audit_name(),
            "warden.isolate_network"
        );
        assert_eq!(
            WardenAction::KillProcessTree {
                cgroup: cgroup("c")?
            }
            .audit_name(),
            "warden.kill_process_tree"
        );
        Ok(())
    }

    // -- Zulässigkeitsmatrix -----------------------------------------------------

    #[test]
    fn test_freeze_and_release_admissible_from_rule_triggered_and_escalated() -> TestResult {
        for action in [
            WardenAction::FreezeCgroup {
                cgroup: cgroup("c")?,
            },
            WardenAction::ReleaseCgroup {
                cgroup: cgroup("c")?,
            },
        ] {
            assert!(action.is_admissible_from(EscalationStage::RuleTriggered));
            assert!(action.is_admissible_from(EscalationStage::Escalated));
        }
        Ok(())
    }

    #[test]
    fn test_isolate_and_kill_admissible_only_from_escalated() -> TestResult {
        for action in [
            WardenAction::IsolateNetwork {
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

    // -- Reversibilität -----------------------------------------------------------

    #[test]
    fn test_reversibility_is_exhaustively_marked_per_action() -> TestResult {
        assert_eq!(
            WardenAction::FreezeCgroup {
                cgroup: cgroup("c")?
            }
            .reversibility(),
            Reversibility::Reversible
        );
        assert_eq!(
            WardenAction::ReleaseCgroup {
                cgroup: cgroup("c")?
            }
            .reversibility(),
            Reversibility::Reversible
        );
        assert_eq!(
            WardenAction::IsolateNetwork {
                cgroup: cgroup("c")?
            }
            .reversibility(),
            Reversibility::Reversible
        );
        assert_eq!(
            WardenAction::KillProcessTree {
                cgroup: cgroup("c")?
            }
            .reversibility(),
            Reversibility::Irreversible
        );
        Ok(())
    }

    // -- content_digest -------------------------------------------------------

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
        for action in [
            WardenAction::FreezeCgroup {
                cgroup: cgroup("c")?,
            },
            WardenAction::ReleaseCgroup {
                cgroup: cgroup("c")?,
            },
            WardenAction::IsolateNetwork {
                cgroup: cgroup("c")?,
            },
            WardenAction::KillProcessTree {
                cgroup: cgroup("c")?,
            },
        ] {
            let value = serde_json::to_value(&action).map_err(ctx("serializes"))?;
            assert_eq!(value["kind"], action.kind_name());
        }
        Ok(())
    }

    #[test]
    fn test_cgroup_returns_target_of_every_variant() -> TestResult {
        let target = cgroup("harw.slice/job-7")?;
        for action in [
            WardenAction::FreezeCgroup {
                cgroup: target.clone(),
            },
            WardenAction::ReleaseCgroup {
                cgroup: target.clone(),
            },
            WardenAction::IsolateNetwork {
                cgroup: target.clone(),
            },
            WardenAction::KillProcessTree {
                cgroup: target.clone(),
            },
        ] {
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
}
