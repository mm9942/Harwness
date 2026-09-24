# Exhaustive Fallunterscheidung — der Compiler findet vergessene Fälle

**Regel:** Fallunterscheidungen über geschlossene Mengen (Enum, Union, sealed Hierarchie, Status, Kombinationen daraus) werden so geschrieben, dass der **Compiler bzw. Typchecker einen fehlenden Fall meldet**. Kein Catch-all (`default`, `else`, `_`), der neue Fälle still schluckt. Nicht unterstützte Kombinationen enden als **Fehlerwert**, nicht als Crash (`unreachable!`, `throw new Error("unreachable")`, `panic`).

**Warum:** Der teuerste Bug ist die neue Enum-Variante, die an 7 von 8 Stellen behandelt wurde. Mit Exhaustivitätsprüfung zeigt der Build **jede** Stelle, die angepasst werden muss — vor dem Deployment, nicht im Incident.

## Wann anwenden

- Du schreibst `switch`/`match`/`when`/if-else-Kaskade über einen Status, Typ-Tag oder Enum.
- Du fügst einer Enum/Union/sealed Hierarchie einen neuen Fall hinzu.
- Du dispatchst über Kombinationen (Algorithmus × Modus, Rolle × Aktion).
- Review findet `default: throw …` oder `_ => unreachable!()`.

## Falsch

```typescript
type Status = "draft" | "submitted" | "approved"; // später kommt "rejected" dazu
function label(s: Status): string {
  switch (s) {
    case "draft": return "Entwurf";
    case "submitted": return "Eingereicht";
    default: return "Freigegeben"; // "rejected" landet still hier
  }
}
```

```rust
match (kem, aead) {
    (Kem::MlKem768, Aead::AesGcmSiv) => seal_768_gcm(pk, pt),
    // … einige Kombinationen …
    _ => unreachable!(), // versteckt fehlende Arme und crasht in Produktion
}
```

## Richtig

### Rust — `match` ohne `_`

```rust
match status {
    Status::Draft => "Entwurf",
    Status::Submitted => "Eingereicht",
    Status::Approved => "Freigegeben",
    // neue Variante → error[E0004]: non-exhaustive patterns
}

// Nicht unterstützt → Fehler statt unreachable!/panic!
// (Kem = {MlKem512, MlKem768}, Aead = {AesGcmSiv, XChaCha20Poly1305}):
match (kem, aead) {
    (Kem::MlKem768, Aead::AesGcmSiv) => seal_768_gcm(pk, pt),
    (Kem::MlKem768, Aead::XChaCha20Poly1305) => seal_768_xchacha(pk, pt),
    (Kem::MlKem512, _) => Err(Error::UnsupportedAlgorithm { detail: format!("{kem:?} + {aead:?}") }),
}
```

### TypeScript — `never`-Check

```typescript
function assertNever(x: never): never {
  throw new Error(`unhandled case: ${JSON.stringify(x)}`); // nur als Laufzeit-Backstop
}

function label(s: Status): string {
  switch (s) {
    case "draft": return "Entwurf";
    case "submitted": return "Eingereicht";
    case "approved": return "Freigegeben";
    default: return assertNever(s); // "rejected" hinzugefügt → Compilefehler hier
  }
}
```

Zusätzlich die ESLint-Regel `@typescript-eslint/switch-exhaustiveness-check` aktivieren. Diskriminierte Unions (`{ kind: "a"; … } | { kind: "b"; … }`) funktionieren genauso über `switch (x.kind)`.

### Python — `match` + `typing.assert_never` (3.11+)

```python
from enum import Enum
from typing import assert_never

class Status(Enum):
    DRAFT = "draft"
    SUBMITTED = "submitted"
    APPROVED = "approved"

def label(s: Status) -> str:
    match s:
        case Status.DRAFT:
            return "Entwurf"
        case Status.SUBMITTED:
            return "Eingereicht"
        case Status.APPROVED:
            return "Freigegeben"
        case _:
            assert_never(s)  # mypy/pyright melden neue Enum-Mitglieder hier
```

Wirksam nur mit Typechecker im CI (mypy/pyright); zur Laufzeit wirft `assert_never` als Backstop.

### Go — Linter statt Compiler

Go prüft `switch` nicht auf Exhaustivität. Den Linter `exhaustive` (github.com/nishanths/exhaustive, auch in golangci-lint) aktivieren und `default` nur für einen **Fehlerwert** nutzen:

```go
func label(s Status) (string, error) {
    switch s {
    case StatusDraft:
        return "Entwurf", nil
    case StatusSubmitted:
        return "Eingereicht", nil
    case StatusApproved:
        return "Freigegeben", nil
    default:
        return "", fmt.Errorf("unknown status %d", s) // kein panic
    }
}
```

### Java 21+ — sealed + switch-Pattern ohne `default`

```java
sealed interface Payment permits Card, Sepa, Invoice {}
record Card(String last4) implements Payment {}
record Sepa(String iban) implements Payment {}
record Invoice(int days) implements Payment {}

String describe(Payment p) {
    return switch (p) {           // kein default → neue Klasse in permits = Compilefehler
        case Card c -> "Karte " + c.last4();
        case Sepa s -> "SEPA " + s.iban();
        case Invoice i -> "Rechnung " + i.days() + " Tage";
    };
}
```

C#: switch expression ohne Discard; Warnung CS8509 als Fehler behandeln (`<WarningsAsErrors>CS8509</WarningsAsErrors>`).

## Wann ist ein Catch-all ok?

Nur bei **offenen** Mengen (Zahlen, Strings, externe Codes), oder wenn „alle übrigen Fälle: nichts tun“ **fachlich** korrekt ist und das auch für künftige Fälle gilt. Dann explizit kommentieren.

## Fallstricke

- **Daten von außen** (JSON, DB, andere Services) können Werte enthalten, die der Typ nicht kennt → an der Grenze parsen und unbekannte Werte als Fehler behandeln; im Inneren exhaustiv matchen.
- **TS `enum` mit numerischen Werten** akzeptiert beliebige Zahlen → String-Literal-Unions bevorzugen.
- **Python ohne Typechecker im CI** → `assert_never` ist nur ein Laufzeit-Crash; Typechecker aktivieren.
- **Kreuzprodukte** mit verschachtelten Switches verlieren die Prüfung — auf Tupel/Paare matchen, wo die Sprache es kann (Rust, Python `match (a, b)`, Java record patterns).
- **`default: throw`** in TS ohne `never`-Typ gibt keinen Compilefehler.

## Checkliste (neue Variante hinzufügen)

1. Variante/Fall in der Definition ergänzen.
2. Build/Typecheck/Linter laufen lassen → Liste aller betroffenen Stellen.
3. Jede Stelle **explizit** erweitern — keinen Catch-all ergänzen.
4. Nicht unterstützte Kombinationen → Fehlerwert mit Detail.
5. Serialisierungs-/Parse-Grenzen prüfen (kennt der Parser den neuen Wert?).
6. Rust zusätzlich: `rust-match-exhaustive`.

## Quelle

- https://doc.rust-lang.org/book/ch06-02-match.html
- https://www.typescriptlang.org/docs/handbook/2/narrowing.html#exhaustiveness-checking
- https://docs.python.org/3/library/typing.html#typing.assert_never
- https://openjdk.org/jeps/441
- https://github.com/nishanths/exhaustive
