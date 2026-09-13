// Diagnostic: ein `String`-Feld – der wichtigste Fall dieser Aufgabe. Ein
// Feld, in das ein Modell freien Text schreiben kann, ist der Weg, auf dem
// eine Modellausgabe zu einem Befehl wird.
// Expected: compile_error! "insbesondere `String` ist nicht erlaubt"
use harw_macros::warden_actions;

warden_actions! {
    authorization_proof = harw_types::CgroupId;

    /// Führt ein beliebiges Kommando aus – genau das, was dieses Makro
    /// verhindern soll.
    RunCommand {
        cmd: String,
        admissible_from: [Escalated],
        audit = "warden.run_command",
    }
}

fn main() {}
