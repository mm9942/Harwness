// Diagnostic: doppelter Aktionsname innerhalb einer Deklaration.
// Expected: compile_error! "bereits doppelt vergeben"
use harw_macros::warden_actions;

warden_actions! {
    authorization_proof = harw_types::CgroupId;

    FreezeCgroup {
        cgroup: harw_types::CgroupId,
        admissible_from: [Escalated],
        audit = "warden.freeze_cgroup",
    }
    FreezeCgroup {
        cgroup: harw_types::CgroupId,
        admissible_from: [RuleTriggered],
        audit = "warden.freeze_cgroup_again",
    }
}

fn main() {}
