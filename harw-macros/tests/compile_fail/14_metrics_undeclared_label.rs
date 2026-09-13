// Diagnostic: a `labels` entry that is not a valid field name.
// Expected: compile_error! ".. enthaelt ungueltiges Zeichen 'C'; erlaubt sind nur [a-z0-9_.]"
harw_macros::metrics! {
    CHILD_ADMITTED: counter, unit = count, labels = ["Clan"], cardinality = single, name = "harw_child_admitted_total";
}

fn main() {}
