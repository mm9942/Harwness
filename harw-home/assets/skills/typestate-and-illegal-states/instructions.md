# Ungültige Zustände unrepräsentierbar machen (Typestate) — sprachunabhängig

**Regel:** Modelliere Lebenszyklen und Berechtigungsstufen so, dass **ungültige Zustände und Übergänge im Typsystem gar nicht ausdrückbar** sind:
- **Ein Typ pro Zustand** (bzw. ein Fall einer diskriminierten Union), der **nur die Felder und Methoden dieses Zustands** hat.
- **Übergänge sind Funktionen `AlterZustand → NeuerZustand`**, die ein neues Objekt liefern; der alte Zustand wird nicht mutiert.
- **„Parse, don't validate“**: Rohdaten werden einmal an der Grenze in einen geprüften Typ überführt (`Email`, `ValidatedInvoice`); danach trägt der Typ den Beweis.
- Boolean-/Nullable-Kombinationen (`isPaid && paidAt == null`) werden durch eine Union/Enum ersetzt.

**Warum:** Laufzeit-Checks (`if (state !== "stored") throw …`) muss jeder Aufrufer richtig machen — und irgendwann vergisst es einer. Wenn `retrieve()` nur auf `StoredObject` existiert, **kompiliert** der falsche Aufruf gar nicht bzw. der Typchecker meldet ihn. Das spart Tests, `panic`-Pfade und Incidents.

## Wann anwenden

- Objekt mit Lebenszyklus: Draft → Submitted → Approved/Rejected, Pending → Stored → Tombstoned, Connecting → Connected → Closed.
- Berechtigungsstufen: Anonymous → Authenticated → Admin.
- Builder mit Pflichtfeldern.
- Strings/Zahlen mit Bedeutung (E-Mail, IBAN, UserId vs. OrgId, Cent vs. Euro), die verwechselt werden können.
- Du willst `isValidated: boolean`, `status: string` + optionale Felder oder `if (state !== …) throw` schreiben.

## Falsch

```typescript
// Flags + optionale Felder: 2^n Kombinationen, die meisten ungültig
interface Upload {
  status: "pending" | "stored" | "tombstoned";
  key: string;
  storedAt?: Date;          // nur bei stored gesetzt … hoffentlich
  deletedReason?: string;   // nur bei tombstoned … hoffentlich
}
function retrieve(u: Upload): string {
  if (u.status !== "stored") throw new Error("wrong state"); // Laufzeit-Check
  return u.key;
}
```

```python
class Invoice:
    def __init__(self):
        self.validated = False      # jeder Aufrufer muss daran denken
    def book(self):
        if not self.validated:
            raise RuntimeError("not validated")
```

## Richtig

### Rust — Marker-Typen + `PhantomData`, Übergänge konsumieren `self`

```rust
use std::marker::PhantomData;

pub struct Pending;
pub struct Stored;

pub struct S3Object<State = Pending> {
    key: String,
    _state: PhantomData<State>,
}

impl S3Object<Pending> {
    pub fn new(key: &str) -> Self { Self { key: key.to_owned(), _state: PhantomData } }
    pub fn mark_stored(self) -> S3Object<Stored> {            // alter Zustand ist danach weg (Move)
        S3Object { key: self.key, _state: PhantomData }
    }
}
impl S3Object<Stored> {
    pub fn retrieve(&self) -> &str { &self.key }              // existiert nur auf Stored
}
```

### TypeScript — diskriminierte Union + Branded Types

```typescript
type PendingUpload = { kind: "pending"; key: string };
type StoredUpload = { kind: "stored"; key: string; storedAt: Date };
type TombstonedUpload = { kind: "tombstoned"; key: string; reason: string };
type Upload = PendingUpload | StoredUpload | TombstonedUpload;

function markStored(u: PendingUpload, at: Date): StoredUpload {
  return { kind: "stored", key: u.key, storedAt: at };
}
function retrieve(u: StoredUpload): string {   // PendingUpload wird vom Compiler abgelehnt
  return u.key;
}

// Branded Type: "geprüft" steckt im Typ
type Email = string & { readonly __brand: "Email" };
function parseEmail(raw: string): Email {
  if (!/^[^@\s]+@[^@\s]+$/.test(raw)) throw new ValidationError("email", raw);
  return raw as Email;                         // einzige Stelle mit Cast
}
function sendInvite(to: Email) { /* … */ }     // sendInvite("x") → Compilefehler
```

### Python — Klasse pro Zustand, frozen dataclasses, `NewType`

```python
from dataclasses import dataclass
from typing import NewType

@dataclass(frozen=True)
class DraftInvoice:
    lines: tuple[Line, ...]
    def validate(self) -> "ValidatedInvoice":
        if not self.lines:
            raise ValidationError("lines", "must not be empty")
        return ValidatedInvoice(self.lines)

@dataclass(frozen=True)
class ValidatedInvoice:
    lines: tuple[Line, ...]
    def book(self, ledger: Ledger) -> "BookedInvoice": ...   # nur hier existiert book()

UserId = NewType("UserId", str)   # mypy/pyright verhindern Verwechslung mit OrgId
OrgId = NewType("OrgId", str)
```

### Go — eigener Typ pro Zustand, Konstruktor als einzige Quelle

```go
type PendingUpload struct{ key string }
type StoredUpload struct {
    key      string
    storedAt time.Time
}

func NewPendingUpload(key string) PendingUpload { return PendingUpload{key: key} }

func (p PendingUpload) MarkStored(at time.Time) StoredUpload {
    return StoredUpload{key: p.key, storedAt: at}
}

func (s StoredUpload) Retrieve() string { return s.key } // PendingUpload hat kein Retrieve

type Email struct{ v string } // unexportiertes Feld: nur ParseEmail erzeugt gültige Werte
func ParseEmail(raw string) (Email, error) { /* prüfen */ return Email{v: raw}, nil }
```

### Java — sealed interface + records

```java
sealed interface Upload permits Pending, Stored, Tombstoned {}
record Pending(String key) implements Upload {
    Stored markStored(Instant at) { return new Stored(key, at); }
}
record Stored(String key, Instant storedAt) implements Upload {
    String retrieve() { return key; }
}
record Tombstoned(String key, String reason) implements Upload {}
```

## Fallstricke

- **GC-Sprachen haben keinen Move**: das alte `PendingUpload`-Objekt ist nach `markStored` noch benutzbar. Darum: Zustandsobjekte **immutable** machen (frozen/readonly/records), sodass Wiederverwendung harmlos ist; bei echten Ressourcen (Verbindung, einmaliger Token) zusätzlich ein Laufzeit-Backstop.
- **Zustand, der aus der DB/JSON kommt**, ist ungeprüft → an der Grenze in den passenden Zustandstyp parsen (Union + `kind`), nicht als „irgendein Upload“ durchreichen.
- **Branded Types mit Cast überall** zerstören den Nutzen — genau **eine** Parse-Funktion darf casten.
- **Overengineering**: für zwei Zustände ohne unterschiedliche Operationen reicht ein Enum-Feld.
- **Übergänge mit `&mut self`/Mutation** (Rust) — dann kann der alte Zustand weiterbenutzt werden; immer `self` by value.
- Exhaustive Behandlung der Union nicht vergessen (siehe `exhaustive-case-handling`).

## Checkliste

1. Welche Zustände/Stufen gibt es? Ist jeder als eigener Typ bzw. Union-Fall modelliert?
2. Hat jeder Zustand nur die Felder, die in ihm garantiert gesetzt sind?
3. Existieren Methoden nur auf den Zuständen, in denen sie erlaubt sind?
4. Liefern Übergänge ein neues Objekt statt zu mutieren (Rust: `self` by value)?
5. Werden Rohdaten genau einmal an der Grenze geparst (`parseX`) statt überall validiert?
6. Sind Zustandsobjekte in GC-Sprachen immutable?
7. Rust zusätzlich: `rust-phantomdata-typestate`, `rust-move-semantics`.

## Quelle

- https://doc.rust-lang.org/std/marker/struct.PhantomData.html
- https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/
- https://www.typescriptlang.org/docs/handbook/2/narrowing.html#discriminated-unions
- https://docs.python.org/3/library/typing.html#newtype
- https://openjdk.org/jeps/409
