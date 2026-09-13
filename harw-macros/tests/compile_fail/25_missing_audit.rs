// Diagnostic: Aktion ohne `audit` – ein Eingriff ohne Spur.
// Expected: compile_error! "hat kein `audit = \"...\"`"
use harw_macros::warden_actions;

warden_actions! {
    authorization_proof = harw_types::CgroupId;

    /// Friert eine cgroup ein.
    FreezeCgroup {
        cgroup: harw_types::CgroupId,
        admissible_from: [Escalated],
    }
}

fn main() {}
