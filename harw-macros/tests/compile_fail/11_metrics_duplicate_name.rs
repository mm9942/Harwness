// Diagnostic: two metrics! entries reusing the same const identifier.
// Expected: compile_error! "Metrik `A` ist in dieser Deklaration bereits doppelt vergeben"
harw_macros::metrics! {
    A: counter, unit = count, labels = [], cardinality = single, name = "harw_a_total";
    A: gauge, unit = count, labels = [], cardinality = single, name = "harw_b";
}

fn main() {}
