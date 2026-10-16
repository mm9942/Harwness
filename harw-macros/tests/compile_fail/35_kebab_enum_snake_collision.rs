// Diagnostic: zwei Varianten kollidieren nach Normalisierung (snake/kebab fallen zusammen).
// Expected: compile_error! "denselben kebab-Namen"
use harw_macros::KebabEnum;

#[derive(KebabEnum)]
#[kebab_enum(case = "snake")]
enum Mode {
    CompleteDrain,
    #[kebab_enum(rename = "complete-drain")]
    Other,
}

fn main() {}
