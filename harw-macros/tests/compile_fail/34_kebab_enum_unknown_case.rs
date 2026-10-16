// Diagnostic: unbekannter `case`-Wert bei KebabEnum.
// Expected: compile_error! "`case` muss"
use harw_macros::KebabEnum;

#[derive(KebabEnum)]
#[kebab_enum(case = "camel")]
enum Mode {
    Chat,
}

fn main() {}
