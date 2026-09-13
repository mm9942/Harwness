// Diagnostic: a counter whose name does not end in `_total`.
// Expected: compile_error! "counter-Metrik `harw_child_admitted` muss auf `_total` enden"
harw_macros::metrics! {
    CHILD_ADMITTED: counter, unit = count, labels = [], cardinality = single, name = "harw_child_admitted";
}

fn main() {}
