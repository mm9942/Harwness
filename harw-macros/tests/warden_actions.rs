//! Positive integration test for `warden_actions!`.
//!
//! Requires `harw-types`, `harw-tools`, `serde` and `serde_json` as
//! dev-dependencies; see `harw-macros/Cargo.toml`.
//!
//! `harw-dod-warden-proto` (AW5-02) does not exist yet, so this test stands
//! in a local `FakeProof` type for the `authorization_proof = <Pfad>;`
//! parameter (see `warden_actions.rs`-Moduldoku, Abschnitt
//! „`AuthorizationProof` — Entscheidung", for why the macro takes this as a
//! caller-supplied path rather than depending on the not-yet-built crate).
//!
//! Three actions are declared, not just the two from the brief's example:
//! `IsolateCgroups` uses a `Vec<CgroupId>` field to prove the grammar also
//! carries the "several targets at once" case the `#[derive(SensorSource)]`
//! builder flagged as a blind spot in his own attribute model (see
//! `warden_actions.rs`-Moduldoku, Abschnitt „Warum `Vec<T>` zusätzlich zum
//! Skalar").
//!
//! This top-level declaration keeps `tool_schema = harw_tools::ToolSpec;`
//! (the case AW5-02 flagged as unusable for a protocol crate under a strict
//! dependency gate) so the pre-existing `tool_schema_is_strict_and_covers_...`
//! test below keeps working unmodified. The [`without_tool_schema`] module
//! covers the opposite, newly-added case — a declaration that omits the key
//! and must never mention `harw_tools` in the generated code — which is the
//! case that unblocks a consumer such as `harw-dod-warden-proto`.
//!
//! # Design-doc reference
//! AW5-01-Brief (`warden_actions!`); AW5-02-Befund („`tool_schema`
//! abwählbar").

use harw_tools::{JsonSchemaType, ToolSpec};
use harw_types::CgroupId;
use serde::{Deserialize, Serialize};

/// Stand-in for `harw_dod_warden_proto::AuthorizationProof` (AW5-02, not yet
/// built). Implements exactly the bounds `warden_actions!` requires of the
/// `authorization_proof = <Pfad>;` type: `Debug`, `Clone`, `Serialize`,
/// `Deserialize`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FakeProof {
    approval_id: String,
}

harw_macros::warden_actions! {
    authorization_proof = FakeProof;
    tool_schema = harw_tools::ToolSpec;

    /// Friert eine cgroup ein.
    FreezeCgroup {
        cgroup: harw_types::CgroupId,
        admissible_from: [RuleTriggered, Escalated],
        audit = "warden.freeze_cgroup",
    }
    /// Beendet einen Prozessbaum.
    KillProcessTree {
        cgroup: harw_types::CgroupId,
        admissible_from: [Escalated],
        audit = "warden.kill_process_tree",
    }
    /// Isoliert mehrere cgroups auf einmal — die "mehrere Ziele"-Form.
    IsolateCgroups {
        cgroups: Vec<harw_types::CgroupId>,
        admissible_from: [Escalated],
        audit = "warden.isolate_cgroups",
    }
}

fn sample_cgroup() -> CgroupId {
    CgroupId::try_from_str("cgroup-1").expect("non-empty id")
}

// -- Wire-Roundtrip -----------------------------------------------------------

#[test]
fn wire_roundtrip_preserves_action_and_proof() {
    let proposed = ProposedAction::FreezeCgroup {
        cgroup: sample_cgroup(),
    };
    let proof = FakeProof {
        approval_id: "approval-42".to_string(),
    };

    let request = WardenActionRequest::new(proposed, proof.clone());
    let json = serde_json::to_string(&request).expect("request must serialize");

    assert!(json.contains("\"kind\":\"freeze-cgroup\""));
    assert!(json.contains("\"approval_id\":\"approval-42\""));

    let round_tripped: WardenActionRequest =
        serde_json::from_str(&json).expect("request must deserialize");

    assert_eq!(round_tripped.action, WardenAction::FreezeCgroup { cgroup: sample_cgroup() });
    assert_eq!(round_tripped.proof, proof);
}

#[test]
fn wire_deserialization_rejects_unknown_fields() {
    let malformed = r#"{"action":{"kind":"freeze-cgroup","cgroup":"cgroup-1","extra":true},"proof":{"approval_id":"a"}}"#;
    let result: Result<WardenActionRequest, _> = serde_json::from_str(malformed);
    assert!(result.is_err(), "deny_unknown_fields must reject the extra field");
}

#[test]
fn vec_field_action_round_trips_through_json() {
    let proposed = ProposedAction::IsolateCgroups {
        cgroups: vec![sample_cgroup(), CgroupId::try_from_str("cgroup-2").unwrap()],
    };
    let action: WardenAction = proposed.into();
    let json = serde_json::to_string(&action).expect("action must serialize");
    let round_tripped: WardenAction = serde_json::from_str(&json).expect("action must deserialize");
    assert_eq!(action, round_tripped);
}

// -- Schema vorhanden ----------------------------------------------------------

#[test]
fn tool_schema_is_strict_and_covers_every_action() {
    let spec = ProposedAction::tool_schema();
    match spec {
        ToolSpec::Function(function) => {
            assert!(function.strict, "the tool schema must be strict");
            let any_of = function
                .parameters
                .any_of
                .expect("parameters must be a union over the three actions");
            assert_eq!(any_of.len(), 3);
            for variant in &any_of {
                assert_eq!(variant.schema_type, Some(JsonSchemaType::Object));
                assert_eq!(
                    variant.additional_properties.as_deref(),
                    Some(&harw_tools::AdditionalProperties::Bool(false))
                );
            }
        }
    }
}

// -- Audit-Name gesetzt ---------------------------------------------------------

#[test]
fn audit_records_the_declared_name_per_action() {
    let freeze = WardenActionAudit::for_action(WardenAction::FreezeCgroup {
        cgroup: sample_cgroup(),
    });
    assert_eq!(freeze.audit_name, "warden.freeze_cgroup");

    let kill = WardenActionAudit::for_action(WardenAction::KillProcessTree {
        cgroup: sample_cgroup(),
    });
    assert_eq!(kill.audit_name, "warden.kill_process_tree");

    let isolate = WardenActionAudit::for_action(WardenAction::IsolateCgroups {
        cgroups: vec![sample_cgroup()],
    });
    assert_eq!(isolate.audit_name, "warden.isolate_cgroups");
}

// -- Zulässigkeitsmatrix befragbar -----------------------------------------------

#[test]
fn admissibility_matrix_matches_the_declaration() {
    let freeze = WardenAction::FreezeCgroup {
        cgroup: sample_cgroup(),
    };
    assert!(freeze.is_admissible_from(EscalationStage::RuleTriggered));
    assert!(freeze.is_admissible_from(EscalationStage::Escalated));

    let kill = WardenAction::KillProcessTree {
        cgroup: sample_cgroup(),
    };
    assert!(!kill.is_admissible_from(EscalationStage::RuleTriggered));
    assert!(kill.is_admissible_from(EscalationStage::Escalated));
}

/// Integration coverage for the case AW5-02 actually needed: a declaration
/// that leaves out `tool_schema = <Pfad>;`.
///
/// This module compiles **without ever writing `harw_tools` anywhere in this
/// file**, and it does not import `harw_tools` at all — proof by absence that
/// the macro's expansion for this declaration does not require the crate.
/// The generated `WardenAction`, `ProposedAction`, `WardenActionRequest`,
/// `WardenActionAudit` and `EscalationStage`/`is_admissible_from` still work
/// exactly as in the top-level module above; only `ProposedAction::tool_schema`
/// does not exist (a stray call to it would fail to compile with "no method
/// named `tool_schema` found", which is exactly the point — see
/// `warden_actions.rs`-Moduldoku, Abschnitt „tool_schema").
///
/// Lives in its own module because `warden_actions!` always emits the same
/// fixed type names (`WardenAction`, `ProposedAction`, ...) — two invocations
/// in the same scope would collide (see `warden_actions.rs`-Moduldoku,
/// Abschnitt „Was pro Deklaration erzeugt wird").
mod without_tool_schema {
    use super::FakeProof;

    harw_macros::warden_actions! {
        authorization_proof = FakeProof;

        /// Friert eine cgroup ein.
        FreezeCgroup {
            cgroup: harw_types::CgroupId,
            admissible_from: [RuleTriggered, Escalated],
            audit = "warden.freeze_cgroup",
        }
        /// Beendet einen Prozessbaum.
        KillProcessTree {
            cgroup: harw_types::CgroupId,
            admissible_from: [Escalated],
            audit = "warden.kill_process_tree",
        }
    }

    fn sample_cgroup() -> harw_types::CgroupId {
        harw_types::CgroupId::try_from_str("cgroup-1").expect("non-empty id")
    }

    #[test]
    fn wire_roundtrip_preserves_action_and_proof_without_tool_schema() {
        let proposed = ProposedAction::FreezeCgroup {
            cgroup: sample_cgroup(),
        };
        let proof = FakeProof {
            approval_id: "approval-42".to_string(),
        };

        let request = WardenActionRequest::new(proposed, proof.clone());
        let json = serde_json::to_string(&request).expect("request must serialize");
        assert!(json.contains("\"kind\":\"freeze-cgroup\""));

        let round_tripped: WardenActionRequest =
            serde_json::from_str(&json).expect("request must deserialize");
        assert_eq!(round_tripped.action, WardenAction::FreezeCgroup { cgroup: sample_cgroup() });
        assert_eq!(round_tripped.proof, proof);
    }

    #[test]
    fn audit_records_the_declared_name_without_tool_schema() {
        let freeze = WardenActionAudit::for_action(WardenAction::FreezeCgroup {
            cgroup: sample_cgroup(),
        });
        assert_eq!(freeze.audit_name, "warden.freeze_cgroup");
    }

    #[test]
    fn admissibility_matrix_matches_the_declaration_without_tool_schema() {
        let kill = WardenAction::KillProcessTree {
            cgroup: sample_cgroup(),
        };
        assert!(!kill.is_admissible_from(EscalationStage::RuleTriggered));
        assert!(kill.is_admissible_from(EscalationStage::Escalated));
    }
}
