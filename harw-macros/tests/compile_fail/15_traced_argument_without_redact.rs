// Diagnose 15: `#[traced(fields(x))]` mit einem Argumenttyp, der `Redact`
// nicht implementiert.
//
// Warum das ein Compile-Fehler sein muss: ein Debug-Abdruck eines Arguments
// zeigt alles, was der Typ hat — Token, Pfade, Modellantworten. `#[traced]`
// erzeugt deshalb `(<arg>).redact()` statt `{:?}` des Rohwerts; ein Typ ohne
// `Redact`-Implementierung darf nicht still auf `Debug` zurückfallen, sondern
// muss den Build brechen.
//
// Erwartet: E0599 "no method named `redact` found for struct `Plain` ..."

struct Plain {
    value: i32,
}

#[harw_macros::traced(fields(x))]
fn use_plain(x: Plain) -> i32 {
    x.value
}

fn main() {}
