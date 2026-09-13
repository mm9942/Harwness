// Diagnostic: `tool_schema = <Pfad>;` doppelt angegeben.
// Expected: compile_error! "`tool_schema` doppelt angegeben"
use harw_macros::warden_actions;

warden_actions! {
    authorization_proof = harw_types::CgroupId;
    tool_schema = harw_tools::ToolSpec;
    tool_schema = harw_tools::ToolSpec;

    FreezeCgroup {
        cgroup: harw_types::CgroupId,
        admissible_from: [Escalated],
        audit = "warden.freeze_cgroup",
    }
}

fn main() {}
