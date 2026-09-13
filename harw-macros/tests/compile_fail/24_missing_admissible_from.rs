// Diagnostic: Aktion ohne `admissible_from` – eine Aktion ohne
// Zulässigkeitsregel ist eine Lücke in der Eskalationsleiter.
// Expected: compile_error! "hat kein `admissible_from: [...]`"
use harw_macros::warden_actions;

warden_actions! {
    authorization_proof = harw_types::CgroupId;

    /// Friert eine cgroup ein.
    FreezeCgroup {
        cgroup: harw_types::CgroupId,
        audit = "warden.freeze_cgroup",
    }
}

fn main() {}
