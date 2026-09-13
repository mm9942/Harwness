// Diagnostic: `tool_schema = "..."` ist syntaktisch kein Pfad.
// Expected: `syn`s eigener Parse-Fehler beim Lesen eines `Path` (kein
// `syn::Error` aus diesem Makro selbst) — der Parser unterscheidet
// syntaktisch ungueltige Pfade (dieser Fall) von syntaktisch gueltigen
// Pfaden, die keinen Typ benennen (z. B. ein Modul- oder Funktionspfad);
// letzteres kann dieses Makro nicht erkennen, siehe
// warden_actions.rs-Moduldoku fuer die Begruendung — dieselbe Grenze gilt
// bereits heute fuer `authorization_proof = <Pfad>;`.
use harw_macros::warden_actions;

warden_actions! {
    authorization_proof = harw_types::CgroupId;
    tool_schema = "not-a-type";

    FreezeCgroup {
        cgroup: harw_types::CgroupId,
        admissible_from: [Escalated],
        audit = "warden.freeze_cgroup",
    }
}

fn main() {}
