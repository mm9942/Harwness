// Diagnostic: ein Feldtyp außerhalb der Positivliste (nicht `String`).
// Expected: compile_error! "ist nicht in der Positivliste"
use harw_macros::warden_actions;

warden_actions! {
    authorization_proof = harw_types::CgroupId;

    /// Setzt einen CPU-Anteil.
    ThrottleCgroup {
        cgroup: harw_types::CgroupId,
        cpu_share: f64,
        admissible_from: [Escalated],
        audit = "warden.throttle_cgroup",
    }
}

fn main() {}
