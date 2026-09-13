// Diagnostic: unit = bytes, but the name does not contain `_bytes`.
// Expected: compile_error! ".. muss `_bytes` im Namen enthalten"
harw_macros::metrics! {
    DATA_READ: counter, unit = bytes, labels = [], cardinality = single, name = "harw_data_read_total";
}

fn main() {}
